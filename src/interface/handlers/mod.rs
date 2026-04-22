//! HTTP handlers grouped by domain.
//!
//! Each submodule owns the handlers for a set of related endpoints. They
//! share `ApiState` and the `json_value` helper from `crate::interface::api`.

pub mod mcp;
pub mod schedules;
pub mod webhooks;
