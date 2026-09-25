use std::collections::HashSet;
use std::sync::Arc;

use crate::config::{Config, LLMModel, LLMProvider, ModelCapability, ProviderType};
use crate::exception::{ChatProviderError, OctopusError};

#[derive(Debug, Clone)]
pub struct LLM {
    pub model_name: String,
    pub max_context_size: usize,
    pub capabilities: HashSet<ModelCapability>,
    pub model_config: Option<LLMModel>,
    pub provider_config: Option<LLMProvider>,
}

pub fn model_display_name(model_name: Option<&str>, model: Option<&LLMModel>) -> String {
    if let Some(m) = model
        && let Some(ref dn) = m.display_name
    {
        return dn.clone();
    }
    match model_name {
        None | Some("") => String::new(),
        Some("kimi-for-coding") | Some("kimi-code") => "kimi-for-coding".to_string(),
        Some(name) => name.to_string(),
    }
}

pub fn augment_provider_with_env_vars(
    provider: &mut LLMProvider,
    model: &mut LLMModel,
) -> Vec<(String, String)> {
    let mut applied = Vec::new();

    match provider.provider_type {
        ProviderType::Kimi => {
            if let Ok(base_url) = std::env::var("KIMI_BASE_URL") {
                provider.base_url = base_url.clone();
                applied.push(("KIMI_BASE_URL".to_string(), base_url));
            }
            if let Ok(api_key) = std::env::var("KIMI_API_KEY") {
                provider.api_key = Some(api_key.clone());
                applied.push(("KIMI_API_KEY".to_string(), "******".to_string()));
            }
            if let Ok(model_name) = std::env::var("KIMI_MODEL_NAME") {
                model.model = model_name.clone();
                applied.push(("KIMI_MODEL_NAME".to_string(), model_name));
            }
            if let Ok(size) = std::env::var("KIMI_MODEL_MAX_CONTEXT_SIZE")
                && let Ok(sz) = size.parse()
            {
                model.max_context_size = sz;
                applied.push(("KIMI_MODEL_MAX_CONTEXT_SIZE".to_string(), size));
            }
            if let Ok(caps) = std::env::var("KIMI_MODEL_CAPABILITIES") {
                let new_caps: Vec<ModelCapability> = caps
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter_map(|s| match s.as_str() {
                        "image_in" => Some(ModelCapability::ImageIn),
                        "video_in" => Some(ModelCapability::VideoIn),
                        "thinking" => Some(ModelCapability::Thinking),
                        "always_thinking" => Some(ModelCapability::AlwaysThinking),
                        _ => None,
                    })
                    .collect();
                if !new_caps.is_empty() {
                    model.capabilities = Some(new_caps);
                    applied.push(("KIMI_MODEL_CAPABILITIES".to_string(), caps));
                }
            }
        }
        ProviderType::OpenaiLegacy | ProviderType::OpenaiResponses => {
            if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                provider.base_url = base_url;
            }
            if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
                provider.api_key = Some(api_key);
            }
        }
        _ => {}
    }

    applied
}

pub fn derive_model_capabilities(model: &LLMModel) -> HashSet<ModelCapability> {
    let mut capabilities = model
        .capabilities
        .clone()
        .map(|c| c.into_iter().collect::<HashSet<_>>())
        .unwrap_or_default();

    let model_lower = model.model.to_lowercase();
    if model_lower.contains("thinking") || model_lower.contains("reason") {
        capabilities.insert(ModelCapability::Thinking);
        capabilities.insert(ModelCapability::AlwaysThinking);
    } else if model.model == "kimi-for-coding" || model.model == "kimi-code" {
        capabilities.insert(ModelCapability::Thinking);
        capabilities.insert(ModelCapability::ImageIn);
        capabilities.insert(ModelCapability::VideoIn);
    }

    capabilities
}

pub fn create_llm(provider: &LLMProvider, model: &LLMModel) -> Option<LLM> {
    let capabilities = derive_model_capabilities(model);
    Some(LLM {
        model_name: model.model.clone(),
        max_context_size: model.max_context_size,
        capabilities,
        model_config: Some(model.clone()),
        provider_config: Some(provider.clone()),
    })
}

pub fn clone_llm_with_model_alias(
    llm: Option<&LLM>,
    config: &Config,
    model_alias: Option<&str>,
) -> crate::exception::Result<Option<LLM>> {
    let alias = match model_alias {
        Some(a) => a,
        None => return Ok(llm.cloned()),
    };
    let model = config.models.get(alias).ok_or_else(|| {
        crate::exception::OctopusError::Other(format!("Unknown model alias: {}", alias))
    })?;
    let provider = config.providers.get(&model.provider).ok_or_else(|| {
        crate::exception::OctopusError::Other(format!("Provider not found: {}", model.provider))
    })?;
    Ok(create_llm(provider, model))
}

#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub id: Option<String>,
    pub message: crate::wire::Message,
    pub usage: Option<crate::wire::TokenUsage>,
    pub tool_calls: Vec<crate::wire::ToolCall>,
}

impl LLM {
    pub async fn complete(
        &self,
        system_prompt: Option<&str>,
        messages: &[crate::wire::Message],
        tools: Option<&[&dyn llm_provider::tooling::CallableTool]>,
    ) -> crate::exception::Result<ChatCompletion> {
        let provider = self.build_provider().await?;
        let llm_history: Vec<llm_provider::Message> =
            messages.iter().map(wire_to_llm_message).collect();
        let llm_tools: Vec<llm_provider::Tool> = tools
            .map(|ts| ts.iter().map(|t| wire_to_llm_tool(*t)).collect())
            .unwrap_or_default();

        let result = llm_provider::generate(
            provider.as_ref(),
            system_prompt.unwrap_or(""),
            &llm_tools,
            &llm_history,
            None,
            None,
        )
        .await
        .map_err(|e| classify_kosong_error(e.to_string()))?;

        let tool_calls = result
            .message
            .tool_calls
            .clone()
            .map(|tcs| tcs.into_iter().map(llm_to_wire_tool_call).collect())
            .unwrap_or_default();

        Ok(ChatCompletion {
            id: result.id,
            message: llm_to_wire_message(result.message),
            usage: result.usage.map(llm_to_wire_usage),
            tool_calls,
        })
    }

    pub async fn generate_streaming<
        'a,
        MP: FnMut(llm_provider::StreamedMessagePart) + Send,
        TC: FnMut(llm_provider::ToolCall) + Send,
    >(
        &'a self,
        system_prompt: Option<&'a str>,
        messages: &'a [crate::wire::Message],
        tools: Option<&'a [&'a dyn llm_provider::tooling::CallableTool]>,
        on_message_part: &'a mut MP,
        on_tool_call: &'a mut TC,
    ) -> crate::exception::Result<ChatCompletion> {
        let provider = self.build_provider().await?;
        let llm_history: Vec<llm_provider::Message> =
            messages.iter().map(wire_to_llm_message).collect();
        let llm_tools: Vec<llm_provider::Tool> = tools
            .map(|ts| ts.iter().map(|t| wire_to_llm_tool(*t)).collect())
            .unwrap_or_default();

        let on_mp: Option<&mut (dyn FnMut(llm_provider::StreamedMessagePart) + Send)> =
            Some(on_message_part);
        let on_tc: Option<&mut (dyn FnMut(llm_provider::ToolCall) + Send)> = Some(on_tool_call);

        let result = llm_provider::generate(
            provider.as_ref(),
            system_prompt.unwrap_or(""),
            &llm_tools,
            &llm_history,
            on_mp,
            on_tc,
        )
        .await
        .map_err(|e| classify_kosong_error(e.to_string()))?;

        let tool_calls = result
            .message
            .tool_calls
            .clone()
            .map(|tcs| tcs.into_iter().map(llm_to_wire_tool_call).collect())
            .unwrap_or_default();

        Ok(ChatCompletion {
            id: result.id,
            message: llm_to_wire_message(result.message),
            usage: result.usage.map(llm_to_wire_usage),
            tool_calls,
        })
    }

    /// Build the LLM provider through the shared `agent-core` factory.
    ///
    /// This replaces the old per-provider builder logic and uses the same
    /// `ProviderType` + `DefaultProviderFactory` path as `Brain`.
    async fn build_provider(
        &self,
    ) -> crate::exception::Result<Arc<dyn llm_provider::ChatProvider>> {
        let provider_config = self
            .provider_config
            .as_ref()
            .ok_or_else(|| OctopusError::Other("Provider config not set".to_string()))?;

        let config = agent_core::BrainConfig {
            base_url: provider_config.base_url.clone(),
            model: self.model_name.clone(),
            provider_type: provider_config.to_agent_core_provider_type(),
            ..Default::default()
        };

        config.build_provider().await.map_err(|e| {
            OctopusError::ChatProvider(ChatProviderError::ProviderError(e.to_string()))
        })
    }
}

// ============================================================================
// Type conversions: wire <-> llm-provider
// ============================================================================

pub(crate) fn wire_to_llm_message(msg: &crate::wire::Message) -> llm_provider::Message {
    llm_provider::Message {
        role: match msg.role.as_str() {
            "system" => llm_provider::Role::System,
            "user" => llm_provider::Role::User,
            "assistant" => llm_provider::Role::Assistant,
            "tool" => llm_provider::Role::Tool,
            _ => llm_provider::Role::User,
        },
        name: None,
        content: msg.content.iter().map(wire_to_llm_content_part).collect(),
        tool_calls: msg
            .tool_calls
            .as_ref()
            .map(|tcs| tcs.iter().map(wire_to_llm_tool_call).collect()),
        tool_call_id: msg.tool_call_id.clone(),
        partial: None,
    }
}

pub(crate) fn wire_to_llm_content_part(
    part: &crate::wire::ContentPart,
) -> llm_provider::ContentPart {
    match part {
        crate::wire::ContentPart::Text { text } => {
            llm_provider::ContentPart::Text { text: text.clone() }
        }
        crate::wire::ContentPart::ImageUrl { image_url } => llm_provider::ContentPart::ImageUrl {
            image_url: llm_provider::ImageUrl {
                url: image_url.url.clone(),
                detail: None,
            },
        },
        crate::wire::ContentPart::AudioUrl { audio_url } => llm_provider::ContentPart::AudioUrl {
            audio_url: llm_provider::AudioUrl {
                url: audio_url.url.clone(),
            },
        },
        crate::wire::ContentPart::VideoUrl { video_url } => llm_provider::ContentPart::VideoUrl {
            video_url: llm_provider::VideoUrl {
                url: video_url.url.clone(),
            },
        },
        crate::wire::ContentPart::Think { think } => llm_provider::ContentPart::Think {
            think: think.clone(),
            encrypted: None,
        },
    }
}

pub(crate) fn wire_to_llm_tool_call(tc: &crate::wire::ToolCall) -> llm_provider::ToolCall {
    llm_provider::ToolCall {
        call_type: tc.call_type,
        id: tc.id.clone(),
        function: llm_provider::FunctionBody {
            name: tc.function.name.clone(),
            arguments: Some(tc.function.arguments.clone()),
        },
        extras: None,
    }
}

pub(crate) fn wire_to_llm_tool(
    tool: &dyn llm_provider::tooling::CallableTool,
) -> llm_provider::Tool {
    llm_provider::Tool {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        parameters: tool.parameters(),
        prompt_fragment: tool.prompt_fragment().map(|s| s.to_string()),
    }
}

pub(crate) fn llm_to_wire_message(msg: llm_provider::Message) -> crate::wire::Message {
    crate::wire::Message {
        role: match msg.role {
            llm_provider::Role::System => "system".to_string(),
            llm_provider::Role::User => "user".to_string(),
            llm_provider::Role::Assistant => "assistant".to_string(),
            llm_provider::Role::Tool => "tool".to_string(),
        },
        content: msg
            .content
            .into_iter()
            .map(llm_to_wire_content_part)
            .collect(),
        tool_call_id: msg.tool_call_id,
        tool_calls: msg
            .tool_calls
            .map(|tcs| tcs.into_iter().map(llm_to_wire_tool_call).collect()),
    }
}

pub(crate) fn llm_to_wire_content_part(
    part: llm_provider::ContentPart,
) -> crate::wire::ContentPart {
    match part {
        llm_provider::ContentPart::Text { text } => crate::wire::ContentPart::Text { text },
        llm_provider::ContentPart::ImageUrl { image_url } => crate::wire::ContentPart::ImageUrl {
            image_url: crate::wire::MediaUrl { url: image_url.url },
        },
        llm_provider::ContentPart::AudioUrl { audio_url } => crate::wire::ContentPart::AudioUrl {
            audio_url: crate::wire::MediaUrl { url: audio_url.url },
        },
        llm_provider::ContentPart::VideoUrl { video_url } => crate::wire::ContentPart::VideoUrl {
            video_url: crate::wire::MediaUrl { url: video_url.url },
        },
        llm_provider::ContentPart::Think { think, .. } => crate::wire::ContentPart::Think { think },
    }
}

pub(crate) fn llm_to_wire_tool_call(tc: llm_provider::ToolCall) -> crate::wire::ToolCall {
    crate::wire::ToolCall {
        id: tc.id,
        call_type: tc.call_type,
        function: crate::wire::ToolCallFunction {
            name: tc.function.name,
            arguments: tc.function.arguments.unwrap_or_default(),
        },
    }
}

pub(crate) fn llm_to_wire_usage(usage: llm_provider::TokenUsage) -> crate::wire::TokenUsage {
    crate::wire::TokenUsage {
        input: usage.input(),
        output: usage.output,
        total: usage.total(),
    }
}

#[allow(dead_code)]
pub(crate) fn llm_to_wire_tool_result(
    result: llm_provider::tooling::ToolResult,
) -> crate::wire::ToolResult {
    crate::wire::ToolResult {
        tool_call_id: result.tool_call_id,
        return_value: crate::wire::ToolReturnValue {
            output: result.return_value.output.and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(crate::wire::ToolOutput::Text(s.to_string()))
                } else {
                    serde_json::from_value::<Vec<crate::wire::ContentPart>>(v.clone())
                        .ok()
                        .map(crate::wire::ToolOutput::Parts)
                        .or_else(|| Some(crate::wire::ToolOutput::Text(v.to_string())))
                }
            }),
            message: result.return_value.message,
            brief: None,
            is_error: result.return_value.is_error,
        },
    }
}

/// Classify an llm-provider error string into a specific OctopusError variant.
pub(crate) fn classify_kosong_error(msg: String) -> crate::exception::OctopusError {
    if msg.starts_with("API connection error:") {
        return crate::exception::OctopusError::APIConnection(
            crate::exception::APIConnectionError(msg),
        );
    }
    if msg.starts_with("API timeout error:") {
        return crate::exception::OctopusError::APITimeout(crate::exception::APITimeoutError(msg));
    }
    if let Some(rest) = msg.strip_prefix("API status error ") {
        // Parse "API status error {status_code}: {message}"
        if let Some(colon_pos) = rest.find(':') {
            let status_part = &rest[..colon_pos];
            if let Ok(status_code) = status_part.parse::<u16>() {
                let message = rest[colon_pos + 2..].to_string(); // skip ": "
                return crate::exception::OctopusError::APIStatus(
                    crate::exception::APIStatusError {
                        status_code,
                        message,
                    },
                );
            }
        }
    }
    if msg == "API returned an empty response" {
        return crate::exception::OctopusError::APIEmptyResponse(
            crate::exception::APIEmptyResponseError,
        );
    }
    crate::exception::OctopusError::Other(msg)
}

impl LLMProvider {
    /// Convert the CLI provider configuration into the shared `agent-core`
    /// provider type.
    ///
    /// Note: `custom_headers` is intentionally not propagated here. The
    /// underlying `llm-provider` OpenAI builders (`OpenAILegacy`,
    /// `OpenAIResponses`) do not expose a custom-header API, so the factory
    /// cannot apply them. Only the Kimi builder supports arbitrary headers,
    /// which are already covered by subscription identity headers.
    pub fn to_agent_core_provider_type(&self) -> agent_core::ProviderType {
        use std::path::PathBuf;

        match self.provider_type {
            ProviderType::Kimi => {
                let token_file = self
                    .oauth
                    .as_ref()
                    .map(|r| crate::auth::oauth::credentials_path(&r.key))
                    .unwrap_or_else(|| PathBuf::from("credentials/kimi-code.json"));
                agent_core::ProviderType::SubscriptionBased {
                    protocol: agent_core::SubscriptionProtocol::Kimi,
                    token_file,
                    identity: agent_core::ProviderIdentity::kimi_code_default(),
                    oauth: agent_core::OAuthConfig::kimi_code(),
                }
            }
            ProviderType::OpenaiLegacy => agent_core::ProviderType::ApiBased {
                protocol: agent_core::ApiProtocol::OpenAiLegacy,
                api_key: self.api_key.clone().unwrap_or_default(),
                reasoning_key: self.reasoning_key.clone(),
            },
            ProviderType::OpenaiResponses => agent_core::ProviderType::ApiBased {
                protocol: agent_core::ApiProtocol::OpenAiResponses,
                api_key: self.api_key.clone().unwrap_or_default(),
                reasoning_key: None,
            },
            _ => agent_core::ProviderType::ApiBased {
                protocol: agent_core::ApiProtocol::OpenAiLegacy,
                api_key: self.api_key.clone().unwrap_or_default(),
                reasoning_key: self.reasoning_key.clone(),
            },
        }
    }
}
