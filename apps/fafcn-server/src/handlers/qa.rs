//! FAF Q&A module backed by `agent-core` and the `faf-units` plugin.

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use agent_core::{
    Brain, BrainBuilder, BrainConfig, BrainError, BrainEvent, ExtismPluginSource, OAuthConfig,
    ProviderIdentity, ProviderType, ToolAwareSystemPromptPolicy,
};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{extract::State, response::IntoResponse, Json};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use crate::{
    config::workspace_root,
    error::{Error, Result},
    state::AppState,
};

/// Configuration for the Q&A agent, loaded from environment variables.
#[derive(Clone, Debug)]
pub struct QaConfig {
    /// LLM backend type and provider-specific credentials.
    pub provider_type: ProviderType,

    /// LLM base URL.
    pub base_url: String,

    /// Model name, e.g. `gpt-4o` or `kimi-for-coding`.
    pub model: String,

    /// Directory containing `.wasm` plugins.
    pub plugins_dir: PathBuf,

    /// System prompt for the agent.
    pub system_prompt: String,

    /// Maximum reasoning steps per turn.
    pub max_steps_per_turn: usize,
}

impl QaConfig {
    /// Load Q&A configuration from environment variables.
    ///
    /// Variables:
    /// - `FAFCN_LLM_PROVIDER_TYPE` — `api` (default) or `subscription`.
    /// - `FAFCN_LLM_BASE_URL` (provider-specific default).
    /// - `FAFCN_LLM_API_KEY` — required for `api` provider.
    /// - `FAFCN_LLM_TOKEN_FILE` — required for `subscription` provider.
    /// - `FAFCN_PLUGINS_DIR` (default: `data/qqbot-data/plugins`).
    /// - `FAFCN_QA_SYSTEM_PROMPT` (optional).
    /// - `FAFCN_QA_MAX_STEPS` (default: `16`).
    pub fn from_env() -> Result<Self> {
        let root = workspace_root();
        let default_plugins = root.join("data/qqbot-data/plugins");
        let default_token_file = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(".kimi/credentials/kimi-code.json"))
            .unwrap_or_else(|_| PathBuf::from(".kimi/credentials/kimi-code.json"));

        let provider_type = match crate::env::var_or("FAFCN_LLM_PROVIDER_TYPE", "api")
            .trim()
            .to_lowercase()
            .as_str()
        {
            "subscription" | "kimi_code" | "kimi-code" => ProviderType::SubscriptionBased {
                protocol: agent_core::SubscriptionProtocol::Kimi,
                token_file: crate::env::path_or("FAFCN_LLM_TOKEN_FILE", default_token_file),
                identity: ProviderIdentity::kimi_code_default(),
                oauth: OAuthConfig::kimi_code(),
            },
            _ => {
                let api_key = crate::env::required("FAFCN_LLM_API_KEY")?;
                let api_key = api_key.trim();
                if api_key.is_empty() {
                    return Err(Error::Config(
                        "FAFCN_LLM_API_KEY cannot be empty for api provider".to_string(),
                    ));
                }
                ProviderType::ApiBased {
                    protocol: agent_core::ApiProtocol::OpenAiLegacy,
                    api_key: api_key.to_string(),
                    reasoning_key: None,
                }
            }
        };

        let model = crate::env::var_or("FAFCN_LLM_MODEL", "gpt-4o");
        let base_url = match &provider_type {
            ProviderType::SubscriptionBased { .. } => {
                crate::env::var_or("FAFCN_LLM_BASE_URL", "https://api.kimi.com/coding/v1")
            }
            ProviderType::ApiBased { .. } => {
                crate::env::var_or("FAFCN_LLM_BASE_URL", "https://api.openai.com/v1")
            }
        };

        let plugins_dir = crate::env::path_or("FAFCN_PLUGINS_DIR", default_plugins);
        let system_prompt = crate::env::var_or(
            "FAFCN_QA_SYSTEM_PROMPT",
            "You are an expert assistant for the game Forged Alliance Forever. \
             Answer questions about units, buildings, and economy using the tools available.",
        );
        let max_steps_per_turn: usize = crate::env::var_or("FAFCN_QA_MAX_STEPS", "16").parse()?;

        let config = Self {
            provider_type,
            base_url,
            model,
            plugins_dir,
            system_prompt,
            max_steps_per_turn,
        };

        // Log the resolved provider configuration.  Secrets are never emitted.
        tracing::info!(
            provider_type = %config.provider_type,
            base_url = %config.base_url,
            model = %config.model,
            plugins_dir = %config.plugins_dir.display(),
            max_steps_per_turn = config.max_steps_per_turn,
            "QaConfig initialized"
        );

        Ok(config)
    }
}

/// Build a `Brain` that loads only the `faf_units_plugin`.
#[tracing::instrument(skip(config))]
pub async fn create_brain(config: &QaConfig) -> Result<Brain> {
    tracing::info!(
        provider_type = %config.provider_type,
        base_url = %config.base_url,
        model = %config.model,
        plugins_dir = %config.plugins_dir.display(),
        "building Q&A brain"
    );
    let allowed: HashSet<String> = ["faf_units_plugin"].into_iter().map(String::from).collect();
    let tool_source = Arc::new(ExtismPluginSource::with_filter(
        &config.plugins_dir,
        allowed,
    ));

    let brain_config = BrainConfig {
        system_prompt: config.system_prompt.clone(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        provider_type: config.provider_type.clone(),
        max_steps_per_turn: config.max_steps_per_turn,
        tool_sources: vec![tool_source],
        ..Default::default()
    };

    Ok(BrainBuilder::default()
        .from_config(brain_config)
        .with_system_prompt_policy(Arc::new(ToolAwareSystemPromptPolicy))
        .build()
        .await?)
}

/// Incoming ask request.
#[derive(Debug, Deserialize)]
pub struct AskRequest {
    pub question: String,
}

/// Event emitted during a Q&A turn.
#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum QaEvent {
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        output: String,
        is_error: bool,
    },
}

/// Response returned by `POST /api/ask`.
#[derive(Debug, Serialize)]
pub struct QaResponse {
    pub answer: String,
    pub events: Vec<QaEvent>,
}

/// Event emitted during a streaming Q&A turn.
#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum QaStreamEvent {
    TextDelta {
        delta: String,
    },
    ThinkingDelta {
        delta: String,
    },
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        output: String,
        is_error: bool,
    },
    Done,
}

/// Run a single question through the agent and collect the answer.
#[tracing::instrument(skip(config))]
pub async fn ask(config: &QaConfig, question: &str) -> Result<QaResponse> {
    tracing::info!(%question, "running Q&A turn");
    let mut brain = create_brain(config).await?;
    let result = brain
        .run_turn_to_completion(question.into())
        .await
        .map_err(|e| Error::Agent(Box::new(BrainError::Other(e.to_string()))))?;

    let events: Vec<QaEvent> = result
        .events
        .into_iter()
        .filter_map(|ev| match ev {
            BrainEvent::ToolCall {
                name, arguments, ..
            } => Some(QaEvent::ToolCall { name, arguments }),
            BrainEvent::ToolResult {
                output, is_error, ..
            } => Some(QaEvent::ToolResult { output, is_error }),
            _ => None,
        })
        .collect();

    tracing::info!(
        event_count = events.len(),
        answer_len = result.final_text.len(),
        "Q&A turn complete"
    );

    Ok(QaResponse {
        answer: result.final_text,
        events,
    })
}

/// Axum handler for `POST /api/ask`.
pub async fn ask_handler(
    State(state): State<AppState>,
    Json(req): Json<AskRequest>,
) -> Result<impl IntoResponse> {
    let resp = ask(&state.qa_config, &req.question).await?;
    Ok(Json(resp))
}

/// Run a single question through the agent and stream the resulting events.
#[tracing::instrument(skip(config))]
pub async fn ask_stream(
    config: &QaConfig,
    question: &str,
) -> Result<impl futures::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut brain = create_brain(config).await.map_err(|e| {
        tracing::error!(error = %e, "failed to build Q&A brain for streaming");
        e
    })?;

    let stream = brain
        .run_turn(question.into())
        .await
        .map_err(|e| Error::Agent(Box::new(BrainError::Other(e.to_string()))))?;

    let event_stream = stream
        .filter_map(|ev| {
            let qa_event = match ev {
                BrainEvent::TextPart(text) => Some(QaStreamEvent::TextDelta { delta: text }),
                BrainEvent::ThinkingPart(text) => {
                    Some(QaStreamEvent::ThinkingDelta { delta: text })
                }
                BrainEvent::ToolCall {
                    name, arguments, ..
                } => Some(QaStreamEvent::ToolCall { name, arguments }),
                BrainEvent::ToolResult {
                    output, is_error, ..
                } => Some(QaStreamEvent::ToolResult { output, is_error }),
                _ => None,
            };
            let event = qa_event.and_then(|e| Event::default().json_data(&e).ok());
            futures::future::ready(event.map(Ok))
        })
        .chain(futures::stream::once(async {
            Ok(Event::default()
                .json_data(&QaStreamEvent::Done)
                .unwrap_or_default())
        }));

    Ok(event_stream)
}

/// Axum handler for `POST /api/ask/stream`.
pub async fn ask_stream_handler(
    State(state): State<AppState>,
    Json(req): Json<AskRequest>,
) -> Result<Sse<impl futures::Stream<Item = std::result::Result<Event, Infallible>>>> {
    let stream = ask_stream(&state.qa_config, &req.question).await?;
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Health check response for `GET /api/health/qa`.
#[derive(Debug, Serialize)]
pub struct QaHealthResponse {
    pub status: &'static str,
    pub provider_type: String,
    pub base_url: String,
    pub model: String,
    pub reply: String,
}

/// Axum handler for `GET /api/health/qa`.
///
/// Performs a tiny provider call to verify that authentication and connectivity
/// are working. Returns `503 Service Unavailable` if the provider rejects the
/// request or cannot be reached.
pub async fn health_handler(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let config = &state.qa_config;
    match verify_provider_auth(config).await {
        Ok(reply) => {
            let body = serde_json::json!(QaHealthResponse {
                status: "ok",
                provider_type: config.provider_type.to_string(),
                base_url: config.base_url.clone(),
                model: config.model.clone(),
                reply,
            });
            Ok((StatusCode::OK, Json(body)))
        }
        Err(e) => {
            let body = serde_json::json!({
                "status": "error",
                "error": e.to_string(),
                "provider_type": config.provider_type.to_string(),
                "base_url": config.base_url,
                "model": config.model,
            });
            Ok((StatusCode::SERVICE_UNAVAILABLE, Json(body)))
        }
    }
}

/// Verify that the configured provider can authenticate and generate a tiny
/// response. This is intended for health checks: it costs a small number of
/// tokens, but proves the API key / OAuth token and base URL are working.
pub async fn verify_provider_auth(config: &QaConfig) -> Result<String> {
    use llm_provider::{ContentPart, Message, Role, StreamedMessagePart, Tool};

    let brain_config = BrainConfig {
        system_prompt: config.system_prompt.clone(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        provider_type: config.provider_type.clone(),
        max_steps_per_turn: config.max_steps_per_turn,
        tool_sources: vec![],
        ..Default::default()
    };

    let provider = brain_config.build_provider().await.map_err(Error::from)?;

    let history = vec![Message {
        role: Role::User,
        name: None,
        content: vec![ContentPart::Text {
            text: "ping".to_string(),
        }],
        tool_calls: None,
        tool_call_id: None,
        partial: None,
    }];

    let system = "You are a helpful assistant. Reply with exactly the word pong.";
    let streamed = provider
        .generate(system, &[] as &[Tool], &history)
        .await
        .map_err(|e| Error::Internal(format!("provider auth / connectivity check failed: {e}")))?;

    let mut reply = String::new();
    let mut stream = streamed.stream;
    while let Some(part) = stream.next().await {
        if let StreamedMessagePart::Content(ContentPart::Text { text }) = part {
            reply.push_str(&text);
        }
    }

    Ok(reply)
}
