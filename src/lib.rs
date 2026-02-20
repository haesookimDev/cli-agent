pub mod agents;
pub mod config;
pub mod context;
pub mod gateway;
pub mod interface;
pub mod mcp;
pub mod memory;
pub mod orchestrator;
pub mod router;
pub mod runtime;
pub mod scheduler;
pub mod types;
pub mod webhook;

pub type AppResult<T> = anyhow::Result<T>;
