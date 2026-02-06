use std::env;
use std::path::PathBuf;

use anyhow::Context;

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
    pub ollama_base_url: String,
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
        let api_secret = env::var("AGENT_API_SECRET")
            .unwrap_or_else(|_| "local-dev-secret".to_string());

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

        let ollama_base_url = env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

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
            ollama_base_url,
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
