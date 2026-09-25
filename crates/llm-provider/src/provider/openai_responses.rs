use crate::chat_provider::{
    ChatProvider, ChatProviderError, Part, StreamedMessage, ThinkingEffort,
};
use crate::message::{ContentPart, FunctionBody, Message, Role, TokenUsage, ToolCall};
use crate::provider::openai_common::{
    convert_reqwest_error, convert_status_error, list_openai_models,
    thinking_effort_to_reasoning_effort,
};
use crate::tooling::Tool;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Provider struct
// ============================================================================

#[derive(Debug, Clone)]
pub struct OpenAIResponses {
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub stream: bool,
    pub thinking: Option<ThinkingEffort>,
    pub generation_kwargs: Value,
    pub http_client: reqwest::Client,
}

impl OpenAIResponses {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: None,
            base_url: "https://api.openai.com/v1".to_string(),
            stream: true,
            thinking: None,
            generation_kwargs: Value::Object(serde_json::Map::new()),
            http_client: reqwest::Client::new(),
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_thinking(mut self, effort: ThinkingEffort) -> Self {
        self.thinking = Some(effort);
        self
    }

    pub fn with_generation_kwargs(mut self, kwargs: Value) -> Self {
        self.generation_kwargs = kwargs;
        self
    }

    pub fn on_retryable_error(&mut self, _error: &ChatProviderError) -> bool {
        self.http_client = reqwest::Client::new();
        true
    }

    fn build_input(&self, system_prompt: &str, history: &[Message]) -> Vec<Value> {
        let mut input = Vec::new();

        if !system_prompt.is_empty() {
            input.push(serde_json::json!({
                "role": "developer",
                "content": system_prompt,
            }));
        }

        for msg in history {
            input.extend(self.convert_message(msg));
        }

        input
    }

    fn convert_message(&self, msg: &Message) -> Vec<Value> {
        let role = match msg.role {
            Role::System => "developer",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        if role == "tool" {
            // Tool results → function_call_output
            let call_id = msg.tool_call_id.clone().unwrap_or_default();
            let output = if msg.content.len() == 1 {
                match &msg.content[0] {
                    ContentPart::Text { text } => Value::String(text.clone()),
                    _ => serde_json::to_value(&msg.content).unwrap_or(Value::Null),
                }
            } else if msg.content.is_empty() {
                Value::String(String::new())
            } else {
                serde_json::to_value(&msg.content).unwrap_or(Value::Null)
            };

            return vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            })];
        }

        let mut result = Vec::new();

        // Convert content parts
        if !msg.content.is_empty() {
            let content_items: Vec<Value> = msg
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } if !text.is_empty() => {
                        Some(serde_json::json!({ "type": "input_text", "text": text }))
                    }
                    ContentPart::ImageUrl { image_url } => Some(serde_json::json!({
                        "type": "input_image",
                        "image_url": image_url.url,
                        "detail": "auto",
                    })),
                    ContentPart::Think { .. } => {
                        // Reasoning items are handled separately below
                        None
                    }
                    _ => None,
                })
                .collect();

            if !content_items.is_empty() {
                result.push(serde_json::json!({
                    "type": "message",
                    "role": role,
                    "content": content_items,
                }));
            }
        }

        // Convert thinking parts to reasoning items
        let think_parts: Vec<&str> = msg
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Think { think, .. } => Some(think.as_str()),
                _ => None,
            })
            .collect();

        if !think_parts.is_empty() {
            let summaries: Vec<Value> = think_parts
                .iter()
                .map(|text| serde_json::json!({ "type": "summary_text", "text": text }))
                .collect();
            result.push(serde_json::json!({
                "type": "reasoning",
                "summary": summaries,
            }));
        }

        // Convert tool calls
        if let Some(ref tcs) = msg.tool_calls {
            for tc in tcs {
                result.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.function.name,
                    "arguments": tc.function.arguments.clone().unwrap_or_default(),
                }));
            }
        }

        result
    }

    fn build_tools(&self, tools: &[Tool]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect()
    }

    fn build_request(&self, system_prompt: &str, tools: &[Tool], history: &[Message]) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert(
            "input".to_string(),
            Value::Array(self.build_input(system_prompt, history)),
        );
        body.insert("store".to_string(), Value::Bool(false));

        if !tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(self.build_tools(tools)));
        }

        if self.stream {
            body.insert("stream".to_string(), Value::Bool(true));
        }

        // Thinking / reasoning
        if let Some(ref effort) = self.thinking
            && let Some(re) = thinking_effort_to_reasoning_effort(effort)
        {
            body.insert(
                "reasoning".to_string(),
                serde_json::json!({
                    "effort": re,
                    "summary": "auto",
                }),
            );
            body.insert(
                "include".to_string(),
                Value::Array(vec![Value::String(
                    "reasoning.encrypted_content".to_string(),
                )]),
            );
        }

        // Merge generation kwargs
        if let Value::Object(ref kwargs) = self.generation_kwargs {
            for (k, v) in kwargs {
                body.insert(k.clone(), v.clone());
            }
        }

        Value::Object(body)
    }
}

#[async_trait]
impl ChatProvider for OpenAIResponses {
    fn name(&self) -> &str {
        "openai-responses"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn thinking_effort(&self) -> Option<&ThinkingEffort> {
        self.thinking.as_ref()
    }

    async fn generate(
        &self,
        system_prompt: &str,
        tools: &[Tool],
        history: &[Message],
    ) -> Result<StreamedMessage, ChatProviderError> {
        let request = self.build_request(system_prompt, tools, history);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let mut req_builder = self.http_client.post(&url);
        if let Some(ref key) = self.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        req_builder = req_builder.header("Content-Type", "application/json");

        let response = req_builder
            .json(&request)
            .send()
            .await
            .map_err(convert_reqwest_error)?;

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(convert_status_error(status, body, request_id));
        }

        if self.stream {
            let stream = create_responses_sse_stream(response);
            Ok(StreamedMessage {
                id: None,
                usage: None,
                stream: Box::pin(stream),
            })
        } else {
            let body = response.text().await.map_err(convert_reqwest_error)?;
            let resp: ResponsesResponse = serde_json::from_str(&body)
                .map_err(|e| ChatProviderError::new(format!("Failed to parse response: {e}")))?;

            let id = Some(resp.id.clone());
            let usage = resp.usage.as_ref().map(|u| TokenUsage {
                input_other: u.input_tokens.saturating_sub(
                    u.input_tokens_details
                        .as_ref()
                        .and_then(|d| d.cached_tokens)
                        .unwrap_or(0) as usize,
                ),
                output: u.output_tokens,
                input_cache_read: u
                    .input_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .unwrap_or(0) as usize,
                input_cache_creation: 0,
            });

            let mut parts = Vec::new();
            for item in &resp.output {
                match item {
                    ResponsesOutputItem::Message { content, .. } => {
                        for c in content {
                            if let Some(text) = c.get("text").and_then(|v| v.as_str()) {
                                parts.push(Part::Content(ContentPart::Text {
                                    text: text.to_string(),
                                }));
                            }
                        }
                    }
                    ResponsesOutputItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        parts.push(Part::ToolCall(ToolCall {
                            call_type: crate::ToolCallType::Function,
                            id: call_id.clone(),
                            function: FunctionBody {
                                name: name.clone(),
                                arguments: Some(arguments.clone()),
                            },
                            extras: None,
                        }));
                    }
                    ResponsesOutputItem::Reasoning { summary, .. } => {
                        for s in summary {
                            if let Some(text) = s.get("text").and_then(|v| v.as_str()) {
                                parts.push(Part::Content(ContentPart::Think {
                                    think: text.to_string(),
                                    encrypted: None,
                                }));
                            }
                        }
                    }
                }
            }

            Ok(StreamedMessage {
                id,
                usage,
                stream: Box::pin(futures::stream::iter(parts)),
            })
        }
    }

    async fn list_models(&self) -> Result<Vec<String>, ChatProviderError> {
        list_openai_models(
            &self.http_client,
            &self.base_url,
            self.api_key.as_deref(),
            &HashMap::new(),
        )
        .await
    }

    fn with_thinking(&self, effort: ThinkingEffort) -> Arc<dyn ChatProvider> {
        let mut cloned = self.clone();
        cloned.thinking = Some(effort);
        Arc::new(cloned)
    }
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct ResponsesResponse {
    pub id: String,
    pub output: Vec<ResponsesOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<Value>,
        #[serde(default)]
        role: String,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        arguments: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        #[serde(default)]
        summary: Vec<Value>,
        #[serde(default)]
        encrypted_content: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<ResponsesInputTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesInputTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<i64>,
}

// ============================================================================
// Streaming
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten)]
    pub data: Value,
}

fn create_responses_sse_stream(response: reqwest::Response) -> BoxStream<'static, Part> {
    struct SseState {
        response: reqwest::Response,
        buffer: String,
    }

    let state = SseState {
        response,
        buffer: String::new(),
    };

    let stream = futures::stream::unfold(state, |mut state| async move {
        loop {
            match state.response.chunk().await {
                Ok(Some(chunk)) => {
                    state.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = state.buffer.find('\n') {
                        let line = state.buffer[..pos].trim().to_string();
                        state.buffer = state.buffer[pos + 1..].to_string();
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                return None;
                            }
                            match serde_json::from_str::<ResponsesStreamEvent>(data) {
                                Ok(event) => {
                                    let parts = event_to_parts(event);
                                    if !parts.is_empty() {
                                        return Some((parts, state));
                                    }
                                }
                                Err(_) => continue,
                            }
                        }
                    }
                }
                Ok(None) => return None,
                Err(_) => return None,
            }
        }
    })
    .flat_map(futures::stream::iter);

    Box::pin(stream)
}

fn event_to_parts(event: ResponsesStreamEvent) -> Vec<Part> {
    let mut parts = Vec::new();
    match event.event_type.as_str() {
        "response.output_text.delta" => {
            if let Some(delta) = event.data.get("delta").and_then(|v| v.as_str())
                && !delta.is_empty()
            {
                parts.push(Part::Content(ContentPart::Text {
                    text: delta.to_string(),
                }));
            }
        }
        "response.output_item.added" => {
            if let Some(item) = event.data.get("item")
                && let Some(item_type) = item.get("type").and_then(|v| v.as_str())
                && item_type == "function_call"
            {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                parts.push(Part::ToolCall(ToolCall {
                    call_type: crate::ToolCallType::Function,
                    id: call_id,
                    function: FunctionBody {
                        name,
                        arguments: Some(arguments),
                    },
                    extras: None,
                }));
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = event.data.get("delta").and_then(|v| v.as_str())
                && !delta.is_empty()
            {
                parts.push(Part::ToolCallPart(crate::message::ToolCallPart {
                    arguments_part: Some(delta.to_string()),
                }));
            }
        }
        "response.reasoning_summary_text.delta" => {
            if let Some(delta) = event.data.get("delta").and_then(|v| v.as_str())
                && !delta.is_empty()
            {
                parts.push(Part::Content(ContentPart::Think {
                    think: delta.to_string(),
                    encrypted: None,
                }));
            }
        }
        "response.completed" => {
            // End of stream; no parts to emit
        }
        _ => {}
    }
    parts
}
