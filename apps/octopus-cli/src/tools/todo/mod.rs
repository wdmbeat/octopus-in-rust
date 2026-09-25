use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::session_state::TodoStatus;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetTodoListParams {
    pub todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    pub title: String,
    pub status: TodoStatus,
}

pub struct SetTodoListTool;

impl Default for SetTodoListTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SetTodoListTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableTool2 for SetTodoListTool {
    type Params = SetTodoListParams;

    fn name(&self) -> &str {
        "SetTodoList"
    }

    fn description(&self) -> &str {
        "Set the todo list for the current session."
    }

    async fn call_typed(&self, params: SetTodoListParams) -> ToolReturnValue {
        ToolReturnValue::ok(format!(
            "Todo list updated with {} items.",
            params.todos.len()
        ))
    }
}
