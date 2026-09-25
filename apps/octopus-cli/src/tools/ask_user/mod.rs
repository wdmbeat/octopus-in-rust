use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserParams {
    pub question: String,
    #[serde(default)]
    pub options: Vec<AskUserOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub struct AskUserTool;

impl Default for AskUserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableTool2 for AskUserTool {
    type Params = AskUserParams;

    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the user a question with structured options."
    }

    async fn call_typed(&self, params: AskUserParams) -> ToolReturnValue {
        println!("\n[ASK USER] {}\n", params.question);
        for (i, opt) in params.options.iter().enumerate() {
            println!("  {}. {}", i + 1, opt.label);
        }
        println!();

        ToolReturnValue::ok(
            "User was asked a question. In interactive mode, the user will respond. In non-interactive mode, this is auto-dismissed.",
        )
    }
}
