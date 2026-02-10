use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use cli_agent::agents::AgentRegistry;
use cli_agent::config::AppConfig;
use cli_agent::context::ContextManager;
use cli_agent::interface::api::{self, ApiState};
use cli_agent::interface::tui;
use cli_agent::memory::MemoryManager;
use cli_agent::orchestrator::Orchestrator;
use cli_agent::router::ModelRouter;
use cli_agent::runtime::AgentRuntime;
use cli_agent::types::{RunRequest, TaskProfile};
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

    let orchestrator = Orchestrator::new(
        AgentRuntime::new(cfg.max_parallelism),
        AgentRegistry::builtin(),
        router,
        memory,
        context,
        webhook,
        cfg.max_graph_depth,
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

            println!("serving on http://{addr}");
            api::serve(addr, ApiState { orchestrator, auth }).await?;
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
