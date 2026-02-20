use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use cli_agent::agents::AgentRegistry;
use cli_agent::config::AppConfig;
use cli_agent::context::ContextManager;
use cli_agent::gateway::discord::{DiscordAdapter, DiscordConfig};
use cli_agent::gateway::slack::{SlackAdapter, SlackConfig};
use cli_agent::gateway::GatewayManager;
use cli_agent::interface::api::{self, ApiState};
use cli_agent::interface::tui;
use cli_agent::memory::MemoryManager;
use cli_agent::orchestrator::Orchestrator;
use cli_agent::router::ModelRouter;
use cli_agent::runtime::AgentRuntime;
use cli_agent::types::{RunRequest, TaskProfile};
use cli_agent::scheduler::CronScheduler;
use cli_agent::webhook::{AuthManager, WebhookDispatcher};

#[derive(Debug, Parser)]
#[command(name = "agent")]
#[command(about = "Rust multi-agent orchestrator with CLI/TUI/API")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(long)]
        task: String,
        #[arg(long, default_value = "general")]
        profile: TaskProfile,
        #[arg(long)]
        session: Option<Uuid>,
        #[arg(long, default_value_t = false)]
        no_wait: bool,
    },
    Tui,
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    Replay {
        #[arg(long)]
        session: Uuid,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Compact {
        #[arg(long)]
        session: Uuid,
    },
    Vacuum,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    let cfg = AppConfig::from_env()?;

    let memory =
        Arc::new(MemoryManager::new(cfg.session_dir.clone(), cfg.database_url.as_str()).await?);
    let router = Arc::new(ModelRouter::new(
        cfg.vllm_base_url.clone(),
        cfg.openai_api_key.clone(),
        cfg.anthropic_api_key.clone(),
        cfg.gemini_api_key.clone(),
    ));
    let context = Arc::new(ContextManager::new(cfg.max_context_tokens));
    let auth = Arc::new(AuthManager::new(
        cfg.api_key.clone(),
        cfg.api_secret.clone(),
        300,
    ));
    let webhook = Arc::new(WebhookDispatcher::new(
        memory.clone(),
        auth.clone(),
        Duration::from_secs(cfg.webhook_timeout_secs),
    ));

    let mut mcp_registry = cli_agent::mcp::McpRegistry::new();
    for server_cfg in &cfg.mcp_servers {
        let args: Vec<&str> = server_cfg.args.split_whitespace().collect();
        match cli_agent::mcp::McpClient::spawn(
            &server_cfg.command,
            &args,
            server_cfg.env.clone(),
        )
        .await
        {
            Ok(client) => {
                if let Err(e) = client.initialize().await {
                    tracing::warn!("MCP '{}' init failed: {e}, skipping", server_cfg.name);
                    continue;
                }
                if let Err(e) = client.discover_tools().await {
                    tracing::warn!("MCP '{}' discover_tools failed: {e}", server_cfg.name);
                }
                let client = Arc::new(client);
                mcp_registry
                    .register(server_cfg.name.clone(), client)
                    .await;
                tracing::info!("MCP server '{}' started", server_cfg.name);
            }
            Err(e) => {
                tracing::warn!("MCP '{}' spawn failed: {e}, skipping", server_cfg.name);
            }
        }
    }
    let mcp = Arc::new(mcp_registry);

    let orchestrator = Orchestrator::new(
        AgentRuntime::new(cfg.max_parallelism),
        AgentRegistry::builtin(),
        router,
        memory,
        context,
        webhook,
        cfg.max_graph_depth,
        mcp,
    );

    match cli.command {
        Commands::Run {
            task,
            profile,
            session,
            no_wait,
        } => {
            let req = RunRequest {
                task,
                profile,
                session_id: session,
                workflow_id: None,
                workflow_params: None,
            };

            if no_wait {
                let submitted = orchestrator.submit_run(req).await?;
                println!(
                    "run_id={} session_id={} status={}",
                    submitted.run_id, submitted.session_id, submitted.status
                );
            } else {
                let result = orchestrator
                    .run_and_wait(req, Duration::from_millis(300))
                    .await?;
                println!("run_id={} status={}", result.run_id, result.status);
                if let Some(err) = result.error {
                    println!("error={}", err);
                }
                for output in result.outputs {
                    println!(
                        "node={} role={} model={} success={} duration_ms={}",
                        output.node_id,
                        output.role,
                        output.model,
                        output.succeeded,
                        output.duration_ms
                    );
                }
            }
        }
        Commands::Tui => {
            let settings_path = cfg.data_dir.join("tui-settings.json");
            tui::run_tui(orchestrator, settings_path).await?;
        }
        Commands::Serve { host, port } => {
            let host = host.unwrap_or(cfg.server_host);
            let port = port.unwrap_or(cfg.server_port);
            let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

            let mut gateway_manager = GatewayManager::new(orchestrator.clone());

            if let Some(slack_cfg) = SlackConfig::from_env() {
                gateway_manager.register_adapter(Arc::new(SlackAdapter::new(slack_cfg)));
                tracing::info!("Slack gateway adapter registered");
            }

            if let Some(discord_cfg) = DiscordConfig::from_env() {
                gateway_manager.register_adapter(Arc::new(DiscordAdapter::new(discord_cfg)));
                tracing::info!("Discord gateway adapter registered");
            }

            let gateway_manager = Arc::new(gateway_manager);
            gateway_manager.start_all_backgrounds().await?;
            let gateway_router = gateway_manager.build_router();

            // Start cron scheduler background task
            let scheduler = CronScheduler::new(memory.clone(), orchestrator.clone());
            tokio::spawn(scheduler.run_loop());

            println!("serving on http://{addr}");
            println!("  web client: http://{addr}/web-client");
            println!("  dashboard:  http://{addr}/dashboard");
            api::serve(addr, ApiState { orchestrator, auth }, Some(gateway_router)).await?;
        }
        Commands::Replay { session } => {
            let events = orchestrator.replay_session(session).await?;
            for event in events {
                println!(
                    "line={} ts={} type={:?} payload={}",
                    event.line, event.event.timestamp, event.event.event_type, event.event.payload
                );
            }
        }
        Commands::Memory { command } => match command {
            MemoryCommand::Compact { session } => {
                orchestrator.compact_session(session).await?;
                println!("session compacted: {session}");
            }
            MemoryCommand::Vacuum => {
                orchestrator.vacuum_memory().await?;
                println!("sqlite vacuum completed");
            }
        },
    }

    Ok(())
}
