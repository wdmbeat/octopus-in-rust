use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchWebParams {
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchURLParams {
    pub url: String,
}

pub struct SearchWebTool;
pub struct FetchURLTool;

impl Default for SearchWebTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchWebTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FetchURLTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FetchURLTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableTool2 for SearchWebTool {
    type Params = SearchWebParams;

    fn name(&self) -> &str {
        "SearchWeb"
    }

    fn description(&self) -> &str {
        "Search the web for information."
    }

    async fn call_typed(&self, params: SearchWebParams) -> ToolReturnValue {
        ToolReturnValue::ok(format!(
            "Web search results for '{}' would appear here.",
            params.query
        ))
    }
}

#[async_trait]
impl CallableTool2 for FetchURLTool {
    type Params = FetchURLParams;

    fn name(&self) -> &str {
        "FetchURL"
    }

    fn description(&self) -> &str {
        "Fetch the content of a web page."
    }

    async fn call_typed(&self, params: FetchURLParams) -> ToolReturnValue {
        let client = reqwest::Client::new();
        let resp = match client.get(&params.url).send().await {
            Ok(r) => r,
            Err(e) => return ToolReturnValue::error(format!("Failed to fetch URL: {}", e)),
        };

        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return ToolReturnValue::error(format!("Failed to read response: {}", e)),
        };

        ToolReturnValue::ok(text)
    }
}
