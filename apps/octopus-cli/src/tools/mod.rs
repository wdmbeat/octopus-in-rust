pub mod agent;
pub mod ask_user;
pub mod background;
pub mod dmail;
pub mod file;
pub mod plan;
pub mod shell;
pub mod think;
pub mod todo;
pub mod tool_name;
pub mod web;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Execution mode for tools that support both synchronous and background operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExecutionMode {
    #[default]
    Foreground,
    Background,
}
