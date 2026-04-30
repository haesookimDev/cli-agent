//! HTTP handlers grouped by domain.
//!
//! Each submodule owns the handlers for a set of related endpoints. They
//! share `ApiState` and the `json_value` helper from `crate::interface::api`.

pub mod agents;
pub mod cluster;
pub mod harness;
pub mod health;
pub mod mcp;
pub mod memory;
pub mod runs;
pub mod schedules;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod team;
pub mod terminal;
pub mod meetings;
pub mod webhooks;
pub mod workflows;
pub mod workspaces;
