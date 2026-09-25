use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnterPlanModeParams {
    #[serde(default)]
    pub reason: String,
}

pub struct EnterPlanModeTool;
pub struct ExitPlanModeTool;

impl Default for EnterPlanModeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EnterPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExitPlanModeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ExitPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableTool2 for EnterPlanModeTool {
    type Params = EnterPlanModeParams;

    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Enter plan mode to create a read-only plan before making changes."
    }

    async fn call_typed(&self, _args: EnterPlanModeParams) -> ToolReturnValue {
        ToolReturnValue::ok("Plan mode activated. All file changes must go through the plan file.")
    }
}

#[async_trait]
impl CallableTool2 for ExitPlanModeTool {
    type Params = ();

    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Exit plan mode and begin executing the plan."
    }

    async fn call_typed(&self, _args: ()) -> ToolReturnValue {
        ToolReturnValue::ok("Plan mode deactivated. You can now make direct changes.")
    }
}
