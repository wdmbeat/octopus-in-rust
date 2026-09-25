use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThinkParams {
    pub thought: String,
}

pub struct ThinkTool;

impl Default for ThinkTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ThinkTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableTool2 for ThinkTool {
    type Params = ThinkParams;

    fn name(&self) -> &str {
        "Think"
    }

    fn description(&self) -> &str {
        "Think through a problem step by step."
    }

    async fn call_typed(&self, params: ThinkParams) -> ToolReturnValue {
        ToolReturnValue::ok(format!("Thought recorded: {}", params.thought))
    }
}
