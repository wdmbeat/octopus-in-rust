use crate::chat_provider::{
    ChatProvider, ChatProviderError, Part, StreamedMessage, ThinkingEffort,
};
use crate::message::{
    ContentPart, FunctionBody, Message, Role, TokenUsage, ToolCall, ToolCallPart,
};
use crate::provider::openai_common::{
    convert_reqwest_error, convert_status_error, list_openai_models,
    thinking_effort_to_reasoning_effort, tool_to_openai,
};
use crate::provider::openai_types::{
    ChatCompletionChunk, ChatCompletionMessage, ChatCompletionRequest, ChatCompletionResponse,
    ChatCompletionTool,
};
use crate::tooling::Tool;
use crate::utils::jsonschema::ensure_property_types;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Kimi {
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub stream: bool,
    pub thinking: Option<ThinkingEffort>,
    pub generation_kwargs: Value,
    pub extra_body: Option<Value>,
    pub headers: HashMap<String, String>,
    pub http_client: reqwest::Client,
}

impl Kimi {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: None,
            base_url: "https://api.moonshot.ai/v1".to_string(),
            stream: true,
            thinking: None,
            generation_kwargs: Value::Object(serde_json::Map::new()),
            extra_body: None,
            headers: HashMap::new(),
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

    pub fn with_extra_body(mut self, extra_body: Value) -> Self {
        self.extra_body = Some(extra_body);
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn on_retryable_error(&mut self, _error: &ChatProviderError) -> bool {
        self.http_client = reqwest::Client::new();
        true
    }

    fn build_messages(
        &self,
        system_prompt: &str,
        history: &[Message],
    ) -> Vec<ChatCompletionMessage> {
        let mut messages = vec![ChatCompletionMessage {
            role: "system".to_string(),
            content: Some(Value::String(system_prompt.to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];

        for msg in history {
            let role = match msg.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };

            let content = if msg.content.len() == 1 {
                match &msg.content[0] {
                    ContentPart::Text { text } => Some(Value::String(text.clone())),
                    _ => Some(serde_json::to_value(&msg.content).unwrap_or(Value::Null)),
                }
            } else if msg.content.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&msg.content).unwrap_or(Value::Null))
            };

            let tool_calls = msg.tool_calls.as_ref().map(|tcs| {
                tcs.iter()
                    .map(|tc| crate::provider::openai_types::ToolCallObject {
                        id: tc.id.clone(),
                        call_type: tc.call_type,
                        function: crate::provider::openai_types::FunctionCallObject {
                            name: Some(tc.function.name.clone()),
                            arguments: tc.function.arguments.clone(),
                        },
                    })
                    .collect()
            });

            messages.push(ChatCompletionMessage {
                role: role.to_string(),
                content,
                name: msg.name.clone(),
                tool_calls,
                tool_call_id: msg.tool_call_id.clone(),
            });
        }

        messages
    }

    fn build_tools(&self, tools: &[Tool]) -> Vec<ChatCompletionTool> {
        tools
            .iter()
            .map(|t| {
                let mut tool = tool_to_openai(t);
                tool.function.parameters = ensure_property_types(&tool.function.parameters);
                tool
            })
            .collect()
    }

    fn build_request(
        &self,
        system_prompt: &str,
        tools: &[Tool],
        history: &[Message],
    ) -> ChatCompletionRequest {
        let mut extra_body = self
            .extra_body
            .clone()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        if let Some(ref effort) = self.thinking
            && let Value::Object(ref mut map) = extra_body
        {
            map.insert("thinking".to_string(), Value::String(effort.clone()));
        }

        let mut request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: self.build_messages(system_prompt, history),
            tools: if tools.is_empty() {
                None
            } else {
                Some(self.build_tools(tools))
            },
            tool_choice: None,
            stream: Some(self.stream),
            reasoning_effort: self
                .thinking
                .as_ref()
                .and_then(|e| thinking_effort_to_reasoning_effort(e)),
            extra_body: if extra_body == Value::Object(serde_json::Map::new()) {
                None
            } else {
                Some(extra_body)
            },
        };

        if let Value::Object(kwargs) = &self.generation_kwargs {
            let req_json =
                serde_json::to_value(&request).unwrap_or(Value::Object(serde_json::Map::new()));
            if let Value::Object(mut req_map) = req_json {
                for (k, v) in kwargs {
                    req_map.insert(k.clone(), v.clone());
                }
                request = serde_json::from_value(Value::Object(req_map)).unwrap_or(request);
            }
        }

        request
    }
}

#[async_trait]
impl ChatProvider for Kimi {
    fn name(&self) -> &str {
        "kimi"
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
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut req_builder = self.http_client.post(&url);
        if let Some(ref key) = self.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        for (name, value) in &self.headers {
            req_builder = req_builder.header(name, value);
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
            let stream = create_sse_stream(response);
            Ok(StreamedMessage {
                id: None,
                usage: None,
                stream: Box::pin(stream),
            })
        } else {
            let body = response.text().await.map_err(convert_reqwest_error)?;
            let completion: ChatCompletionResponse = serde_json::from_str(&body)
                .map_err(|e| ChatProviderError::new(format!("Failed to parse response: {e}")))?;

            let id = Some(completion.id.clone());
            let usage = completion.usage.as_ref().map(|u| TokenUsage {
                input_other: u.prompt_tokens.max(0) as usize,
                output: u.completion_tokens.max(0) as usize,
                input_cache_read: u
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .unwrap_or(0) as usize,
                input_cache_creation: 0,
            });

            let mut parts = Vec::new();
            if let Some(choice) = completion.choices.first() {
                if let Some(Value::String(content)) = &choice.message.content {
                    parts.push(Part::Content(ContentPart::Text {
                        text: content.clone(),
                    }));
                }
                if let Some(tool_calls) = &choice.message.tool_calls {
                    for tc in tool_calls {
                        parts.push(Part::ToolCall(ToolCall {
                            call_type: tc.call_type,
                            id: tc.id.clone(),
                            function: FunctionBody {
                                name: tc.function.name.clone().unwrap_or_default(),
                                arguments: tc.function.arguments.clone(),
                            },
                            extras: None,
                        }));
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
            &self.headers,
        )
        .await
    }

    fn with_thinking(&self, effort: ThinkingEffort) -> Arc<dyn ChatProvider> {
        let mut cloned = self.clone();
        cloned.thinking = Some(effort);
        Arc::new(cloned)
    }
}

pub fn create_sse_stream(response: reqwest::Response) -> BoxStream<'static, Part> {
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
                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => {
                                    let parts = chunk_to_parts(chunk);
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

fn chunk_to_parts(chunk: ChatCompletionChunk) -> Vec<Part> {
    let mut parts = Vec::new();
    for choice in chunk.choices {
        let delta = choice.delta;
        if let Some(Value::String(content)) = delta.content
            && !content.is_empty()
        {
            parts.push(Part::Content(ContentPart::Text { text: content }));
        }
        if let Some(tool_calls) = delta.tool_calls {
            for tc in tool_calls {
                if let Some(name) = tc.function.name {
                    parts.push(Part::ToolCall(ToolCall {
                        call_type: tc.call_type,
                        id: tc.id.clone(),
                        function: FunctionBody {
                            name,
                            arguments: tc.function.arguments.clone(),
                        },
                        extras: None,
                    }));
                } else if let Some(args) = tc.function.arguments
                    && !args.is_empty()
                {
                    parts.push(Part::ToolCallPart(ToolCallPart {
                        arguments_part: Some(args),
                    }));
                }
            }
        }
    }
    parts
}
