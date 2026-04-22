//! HTTP handlers grouped by domain.
//!
//! Each submodule owns the handlers for a set of related endpoints. They
//! share `ApiState` and the `json_value` helper from `crate::interface::api`.

pub mod cluster;
pub mod mcp;
pub mod memory;
pub mod schedules;
pub mod settings;
pub mod skills;
pub mod team;
pub mod terminal;
pub mod webhooks;
pub mod workflows;
