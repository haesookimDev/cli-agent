use std::env;
use std::path::PathBuf;

use anyhow::Context;

use crate::types::{CoderBackendKind, McpServerConfig};

/// Substitutes `${VAR_NAME}` patterns with environment variable values.
/// Unset variables are replaced with an empty string.
fn expand_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let value = env::var(var_name).unwrap_or_default();
            result = format!("{}{}{}", &result[..start], value, &result[start + end + 1..]);
        } else {
            break;
        }
    }
    result
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub session_dir: PathBuf,
    pub database_url: String,
    pub api_key: String,
    pub api_secret: String,
    pub server_host: String,
    pub server_port: u16,
    pub max_parallelism: usize,
    pub max_graph_depth: u8,
    pub webhook_timeout_secs: u64,
    pub max_context_tokens: usize,
    pub vllm_base_url: String,
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub gemini_api_key: Option<String>,
    pub github_token: Option<String>,
    pub mcp_enabled: bool,
    pub mcp_servers: Vec<McpServerConfig>,
    pub coder_backend: CoderBackendKind,
    pub coder_command: String,
    pub coder_args: Vec<String>,
    pub coder_working_dir: Option<String>,
    pub coder_timeout_ms: u64,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir = env::var("AGENT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));
        let session_dir = data_dir.join("sessions");
        let database_path = data_dir.join("agent.db");
        let database_url = env::var("AGENT_DATABASE_URL")
            .unwrap_or_else(|_| format!("sqlite://{}", database_path.display()));

        let api_key = env::var("AGENT_API_KEY").unwrap_or_else(|_| "local-dev-key".to_string());
        let api_secret =
            env::var("AGENT_API_SECRET").unwrap_or_else(|_| "local-dev-secret".to_string());

        let server_host = env::var("AGENT_SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let server_port = env::var("AGENT_SERVER_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8080);

        let max_parallelism = env::var("AGENT_MAX_PARALLELISM")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(8);

        let max_graph_depth = env::var("AGENT_MAX_GRAPH_DEPTH")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(6);

        let webhook_timeout_secs = env::var("AGENT_WEBHOOK_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5);

        let max_context_tokens = env::var("AGENT_MAX_CONTEXT_TOKENS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(16_000);

        let vllm_base_url =
            env::var("VLLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".to_string());

        let openai_api_key = env::var("OPENAI_API_KEY").ok();
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY").ok();
        let gemini_api_key = env::var("GEMINI_API_KEY").ok();
        let github_token = env::var("GITHUB_TOKEN").ok();

        let mcp_enabled = env::var("MCP_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let mcp_servers = if mcp_enabled {
            let config_path =
                env::var("MCP_CONFIG_PATH").unwrap_or_else(|_| "mcp_servers.json".to_string());
            match std::fs::read_to_string(&config_path) {
                Ok(json) => {
                    let mut servers: Vec<McpServerConfig> =
                        serde_json::from_str(&json).unwrap_or_else(|e| {
                            eprintln!("warn: failed to parse {}: {}", config_path, e);
                            Vec::new()
                        });
                    for server in &mut servers {
                        server.args = expand_env_vars(&server.args);
                        server.env = server
                            .env
                            .iter()
                            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
                            .collect();
                    }
                    servers
                }
                Err(e) => {
                    eprintln!("warn: failed to read {}: {}", config_path, e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let coder_backend = match env::var("CODER_BACKEND")
            .unwrap_or_else(|_| "llm".to_string())
            .to_lowercase()
            .as_str()
        {
            "claude_code" => CoderBackendKind::ClaudeCode,
            "codex" => CoderBackendKind::Codex,
            _ => CoderBackendKind::Llm,
        };
        let coder_command =
            env::var("CODER_COMMAND").unwrap_or_else(|_| "claude".to_string());
        let coder_args: Vec<String> = env::var("CODER_ARGS")
            .ok()
            .map(|v| shell_words::split(&v).unwrap_or_else(|_| vec![v]))
            .unwrap_or_default();
        let coder_working_dir = env::var("CODER_WORKING_DIR").ok();
        let coder_timeout_ms = env::var("CODER_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300_000);

        let cfg = Self {
            data_dir,
            session_dir,
            database_url,
            api_key,
            api_secret,
            server_host,
            server_port,
            max_parallelism,
            max_graph_depth,
            webhook_timeout_secs,
            max_context_tokens,
            vllm_base_url,
            openai_api_key,
            anthropic_api_key,
            gemini_api_key,
            github_token,
            mcp_enabled,
            mcp_servers,
            coder_backend,
            coder_command,
            coder_args,
            coder_working_dir,
            coder_timeout_ms,
        };

        cfg.ensure_dirs()?;
        Ok(cfg)
    }

    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("failed to create data dir {}", self.data_dir.display()))?;
        std::fs::create_dir_all(&self.session_dir).with_context(|| {
            format!(
                "failed to create session dir {}",
                self.session_dir.display()
            )
        })?;
        Ok(())
    }

    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.server_host, self.server_port)
    }
}
