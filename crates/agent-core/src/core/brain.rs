use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures::{Stream, StreamExt};
use llm_provider::Toolset;
use llm_provider::message::{ContentPart, Message, Role};
use llm_provider::tooling::{HandleResult, ToolReturnValue};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::core::approval::{ApprovalRequest, ApprovalResponse, ApprovalRuntime};
use crate::core::config::BrainConfig;
use crate::core::errors::BrainError;
use crate::core::events::{BrainEvent, ProviderRefreshSender};
use crate::core::recovery::RecoveryAction;
use crate::core::registry::ToolRegistry;
use crate::core::step::{StepContext, StepControl, StepOutcome};
use crate::core::turn::{TurnInput, TurnResult};
use crate::hooks::policy::HookAction;

/// A reusable agent core.
#[derive(Clone)]
pub struct Brain {
    config: BrainConfig,
    provider: Arc<dyn llm_provider::ChatProvider>,
    registry: ToolRegistry,
    custom_toolset: Option<Arc<dyn llm_provider::Toolset>>,
}

impl Brain {
    /// Create a new Brain from configuration.
    ///
    /// This constructor is synchronous and expects a pre-built provider in
    /// `config.provider`. For async construction from a factory, use
    /// [`BrainBuilder`](crate::core::builder::BrainBuilder).
    pub fn new(config: BrainConfig) -> Result<Self, BrainError> {
        let provider = config.provider.clone().ok_or(BrainError::NoProvider)?;
        let registry = ToolRegistry::new();
        let custom_toolset = config.toolset.clone();

        if custom_toolset.is_none() {
            for source in &config.tool_sources {
                for (source_label, tool) in source.load_tools() {
                    let name = tool.name().to_string();
                    if registry.find(&name).is_some() {
                        tracing::warn!(
                            "Tool '{}' from source '{}' conflicts with an existing tool, skipping",
                            name,
                            source_label
                        );
                        continue;
                    }
                    registry.register(tool, source_label.clone());
                    tracing::info!("Registered tool '{}' from source '{}'", name, source_label);
                }
            }
        }

        Ok(Self {
            config,
            provider,
            registry,
            custom_toolset,
        })
    }

    /// Access the underlying tool registry.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Access the message store.
    pub fn message_store(
        &self,
    ) -> &Arc<tokio::sync::Mutex<dyn crate::session::store::MessageStore>> {
        &self.config.message_store
    }

    /// Run a single user turn and return a stream of events.
    pub async fn run_turn(
        &mut self,
        input: TurnInput,
    ) -> Result<Pin<Box<dyn Stream<Item = BrainEvent> + Send>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let config = self.config.clone();
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        let custom_toolset = self.custom_toolset.clone();

        tokio::spawn(async move {
            run_turn_loop(config, provider, registry, custom_toolset, input, tx).await;
        });

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }

    /// Run a single user turn to completion (non-streaming).
    pub async fn run_turn_to_completion(&mut self, input: TurnInput) -> Result<TurnResult> {
        let mut stream = self.run_turn(input).await?;
        let mut events = Vec::new();
        let mut final_text = String::new();

        while let Some(event) = stream.next().await {
            match &event {
                BrainEvent::TextPart(text) => final_text.push_str(text),
                BrainEvent::ToolCall { .. } => final_text.clear(),
                _ => {}
            }
            events.push(event);
        }

        Ok(TurnResult { events, final_text })
    }

    /// Register an additional tool at runtime (e.g. a host-provided tool).
    pub fn register_tool(
        &mut self,
        tool: Box<dyn llm_provider::tooling::CallableTool>,
        source: impl Into<String>,
    ) {
        let name = tool.name().to_string();
        if self.registry.find(&name).is_some() {
            tracing::warn!("Tool '{}' already registered, skipping", name);
            return;
        }
        let source = source.into();
        self.registry.register(tool, source.clone());
        tracing::info!("Registered tool '{}' from source '{}'", name, source);
    }

    /// Run a single reasoning step and return a stream of events.
    ///
    /// Unlike [`Self::run_turn`], this does not push a user message or emit
    /// `TurnBegin`/`TurnEnd`. It assumes the caller has already seeded the
    /// message store and manages the outer turn loop.
    pub async fn run_step(&mut self) -> Result<Pin<Box<dyn Stream<Item = BrainEvent> + Send>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let config = self.config.clone();
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        let custom_toolset = self.custom_toolset.clone();

        tokio::spawn(async move {
            run_step_loop(config, provider, registry, custom_toolset, tx).await;
        });

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }

    /// Replace the current system prompt.
    pub fn set_system_prompt(&mut self, prompt: String) {
        self.config.system_prompt = prompt;
    }

    /// Replace the underlying chat provider (e.g. after an OAuth token refresh).
    pub fn set_provider(&mut self, provider: Arc<dyn llm_provider::ChatProvider>) {
        self.provider = provider;
    }
}

fn emit(tx: &UnboundedSender<BrainEvent>, event: BrainEvent) {
    let _ = tx.send(event);
}

async fn run_turn_loop(
    config: BrainConfig,
    provider: Arc<dyn llm_provider::ChatProvider>,
    registry: ToolRegistry,
    custom_toolset: Option<Arc<dyn llm_provider::Toolset>>,
    input: TurnInput,
    tx: UnboundedSender<BrainEvent>,
) {
    emit(&tx, BrainEvent::TurnBegin);

    if let HookAction::Block { reason } = config
        .hook_policy
        .on_user_prompt_submit(&input.user_message)
        .await
    {
        emit(&tx, BrainEvent::Error(reason.clone()));
        emit(&tx, BrainEvent::TurnEnd);
        config.hook_policy.on_turn_failure(&reason).await;
        return;
    }

    {
        let mut store = config.message_store.lock().await;
        store
            .push(Message {
                role: Role::User,
                name: None,
                content: vec![ContentPart::Text {
                    text: input.user_message,
                }],
                tool_calls: None,
                tool_call_id: None,
                partial: None,
            })
            .await;
    }

    let base_toolset: Arc<dyn llm_provider::Toolset> = match custom_toolset {
        Some(toolset) => toolset,
        None => Arc::new(ApprovalToolset {
            inner: registry.clone(),
            runtime: config.approval_runtime.clone(),
            event_tx: tx.clone(),
        }),
    };
    let toolset = HookAwareToolset {
        inner: base_toolset,
        hook_policy: config.hook_policy.clone(),
    };

    let mut final_text = String::new();

    for step_no in 0..config.max_steps_per_turn {
        let ctx = StepContext {
            step_no,
            turn_id: None,
        };

        emit(&tx, BrainEvent::StepBegin { n: step_no });

        // Create a checkpoint before the step, if a checkpoint policy is configured.
        if let Some(policy) = &config.checkpoint_policy {
            let history = config.message_store.lock().await.history().await;
            match policy.checkpoint(&ctx, &history).await {
                Ok(id) => emit(&tx, BrainEvent::CheckpointCreated { id }),
                Err(e) => {
                    emit(&tx, BrainEvent::Error(e.to_string()));
                    emit(&tx, BrainEvent::TurnEnd);
                    config.hook_policy.on_turn_failure(&e.to_string()).await;
                    return;
                }
            }
        }

        match run_single_step_with_retry(&config, &toolset, &ctx, provider.clone(), tx.clone())
            .await
        {
            Ok(StepControl::Continue) => {
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                continue;
            }
            Ok(StepControl::Stop { final_text: text }) => {
                final_text = text;
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                break;
            }
            Ok(StepControl::RewindToCheckpoint {
                checkpoint_id,
                inject_messages,
            }) => {
                if let Some(policy) = &config.checkpoint_policy {
                    match policy.revert_to(checkpoint_id).await {
                        Ok(history) => {
                            let mut store = config.message_store.lock().await;
                            let mut new_history = history;
                            new_history.extend(inject_messages);
                            store.set_history(new_history).await;
                            emit(&tx, BrainEvent::CheckpointReverted { id: checkpoint_id });
                        }
                        Err(e) => {
                            emit(&tx, BrainEvent::Error(e.to_string()));
                            emit(&tx, BrainEvent::TurnEnd);
                            config.hook_policy.on_turn_failure(&e.to_string()).await;
                            return;
                        }
                    }
                }
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                continue;
            }
            Err(err) => {
                let msg = err.to_string();
                emit(&tx, BrainEvent::Error(msg.clone()));
                emit(&tx, BrainEvent::StepInterrupted);
                emit(&tx, BrainEvent::TurnEnd);
                config.hook_policy.on_turn_failure(&msg).await;
                return;
            }
        }
    }

    if final_text.is_empty() {
        let error = format!(
            "Turn exceeded maximum steps ({})",
            config.max_steps_per_turn
        );
        emit(&tx, BrainEvent::Error(error.clone()));
        emit(&tx, BrainEvent::TurnEnd);
        config.hook_policy.on_turn_failure(&error).await;
        return;
    }

    emit(&tx, BrainEvent::TurnEnd);
    config.hook_policy.on_turn_end(&final_text).await;
}

async fn run_step_loop(
    config: BrainConfig,
    provider: Arc<dyn llm_provider::ChatProvider>,
    registry: ToolRegistry,
    custom_toolset: Option<Arc<dyn llm_provider::Toolset>>,
    tx: UnboundedSender<BrainEvent>,
) {
    let base_toolset: Arc<dyn llm_provider::Toolset> = match custom_toolset {
        Some(toolset) => toolset,
        None => Arc::new(ApprovalToolset {
            inner: registry.clone(),
            runtime: config.approval_runtime.clone(),
            event_tx: tx.clone(),
        }),
    };
    let toolset = HookAwareToolset {
        inner: base_toolset,
        hook_policy: config.hook_policy.clone(),
    };

    let step_no = 0;
    loop {
        let ctx = StepContext {
            step_no,
            turn_id: None,
        };

        // Create a checkpoint before the step, if a checkpoint policy is configured.
        if let Some(policy) = &config.checkpoint_policy {
            let history = config.message_store.lock().await.history().await;
            match policy.checkpoint(&ctx, &history).await {
                Ok(id) => emit(&tx, BrainEvent::CheckpointCreated { id }),
                Err(e) => {
                    emit(&tx, BrainEvent::Error(e.to_string()));
                    return;
                }
            }
        }

        match run_single_step_with_retry(&config, &toolset, &ctx, provider.clone(), tx.clone())
            .await
        {
            Ok(StepControl::Continue) => {
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                break;
            }
            Ok(StepControl::Stop { final_text: _ }) => {
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                break;
            }
            Ok(StepControl::RewindToCheckpoint {
                checkpoint_id,
                inject_messages,
            }) => {
                if let Some(policy) = &config.checkpoint_policy {
                    match policy.revert_to(checkpoint_id).await {
                        Ok(history) => {
                            let mut store = config.message_store.lock().await;
                            let mut new_history = history;
                            new_history.extend(inject_messages);
                            store.set_history(new_history).await;
                            emit(&tx, BrainEvent::CheckpointReverted { id: checkpoint_id });
                        }
                        Err(e) => {
                            emit(&tx, BrainEvent::Error(e.to_string()));
                            return;
                        }
                    }
                }
                emit(&tx, BrainEvent::StepEnd { n: step_no });
                continue;
            }
            Err(err) => {
                emit(&tx, BrainEvent::Error(err.to_string()));
                return;
            }
        }
    }
}

async fn run_single_step_with_retry(
    config: &BrainConfig,
    toolset: &HookAwareToolset,
    ctx: &StepContext,
    mut provider: Arc<dyn llm_provider::ChatProvider>,
    tx: UnboundedSender<BrainEvent>,
) -> Result<StepControl, BrainError> {
    let mut attempt: usize = 0;

    loop {
        attempt += 1;

        match execute_step(config, toolset, ctx, provider.clone(), tx.clone()).await {
            Ok(control) => return Ok(control),
            Err(err) => {
                if attempt < config.retry_policy.max_attempts()
                    && let Some(wait) = config.retry_policy.should_retry(&err, attempt)
                {
                    emit(
                        &tx,
                        BrainEvent::StepRetry {
                            n: ctx.step_no,
                            next_attempt: attempt + 1,
                            max_attempts: config.retry_policy.max_attempts(),
                            wait_s: wait.as_secs_f64(),
                            error_type: err.category(),
                            status_code: err.status_code(),
                        },
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }

                match config.recovery_policy.recover(&err).await {
                    RecoveryAction::RefreshProvider => {
                        emit(
                            &tx,
                            BrainEvent::ProviderRefreshing {
                                reason: format!("recovering from {}", err),
                            },
                        );
                        match config.build_provider().await {
                            Ok(new_provider) => {
                                provider = new_provider;
                                attempt = 0;
                                emit(&tx, BrainEvent::ProviderRefreshed);
                                continue;
                            }
                            Err(build_err) => {
                                return Err(BrainError::Recovery(format!(
                                    "failed to refresh provider: {build_err}"
                                )));
                            }
                        }
                    }
                    RecoveryAction::RequestInteractiveProvider { reason } => {
                        let (refresh_tx, mut refresh_rx) = tokio::sync::mpsc::unbounded_channel();
                        emit(
                            &tx,
                            BrainEvent::ProviderRefreshRequested {
                                reason,
                                sender: ProviderRefreshSender(refresh_tx),
                            },
                        );
                        match refresh_rx.recv().await {
                            Some(new_provider) => {
                                provider = new_provider;
                                attempt = 0;
                                emit(&tx, BrainEvent::ProviderRefreshed);
                                continue;
                            }
                            None => {
                                return Err(BrainError::Recovery(
                                    "interactive provider refresh cancelled".to_string(),
                                ));
                            }
                        }
                    }
                    RecoveryAction::Retry { wait } => {
                        tokio::time::sleep(wait).await;
                        attempt = 0;
                        continue;
                    }
                    RecoveryAction::Abort => return Err(err),
                }
            }
        }
    }
}

async fn execute_step(
    config: &BrainConfig,
    toolset: &HookAwareToolset,
    ctx: &StepContext,
    provider: Arc<dyn llm_provider::ChatProvider>,
    tx: UnboundedSender<BrainEvent>,
) -> Result<StepControl, BrainError> {
    // Build the effective history for this step.
    let base_history = config.message_store.lock().await.history().await;

    // Optional compaction.
    let base_history = if let Some(policy) = &config.compaction_policy {
        if let Some(compacted) = policy.maybe_compact(&base_history).await {
            config
                .message_store
                .lock()
                .await
                .set_history(compacted.clone())
                .await;
            compacted
        } else {
            base_history
        }
    } else {
        base_history
    };

    let mut step_history = base_history.clone();

    // Optional dynamic injection (appended to history and persisted).
    if let Some(policy) = &config.injection_policy {
        let injected = policy.inject(&step_history).await;
        step_history.extend(injected.clone());
        {
            let mut store = config.message_store.lock().await;
            for m in injected {
                store.push(m).await;
            }
        }
    }

    // Step lifecycle hook: before_step. Any additions are persisted so that
    // reminders, notifications, and steers survive in the context file.
    let before_step_len = step_history.len();
    if let Some(policy) = &config.step_policy {
        policy.before_step(ctx, &mut step_history).await?;
    }
    {
        let mut store = config.message_store.lock().await;
        for m in step_history.iter().skip(before_step_len) {
            store.push(m.clone()).await;
        }
    }

    // Build effective system prompt.
    let tools = toolset.tools();
    let system_prompt = config
        .system_prompt_policy
        .build_prompt(&config.system_prompt, &tools, &step_history)
        .await?;

    let step_result = match llm_provider::step(
        provider.as_ref(),
        &system_prompt,
        toolset,
        &step_history,
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return Err(BrainError::from_kosong_error(e)),
    };

    if let Some(usage) = step_result.usage.clone() {
        emit(&tx, BrainEvent::Usage { usage });
    }

    // Extract text / thinking / tool-call events from the assistant message.
    let assistant_message = step_result.message.clone();
    let mut assistant_text = String::new();
    for part in &assistant_message.content {
        match part {
            ContentPart::Text { text } => {
                emit(&tx, BrainEvent::TextPart(text.clone()));
                assistant_text.push_str(text);
            }
            ContentPart::Think { think, .. } => {
                emit(&tx, BrainEvent::ThinkingPart(think.clone()));
            }
            _ => {}
        }
    }

    // If the assistant requested tool calls, emit events and execute them.
    if let Some(ref tool_calls) = assistant_message.tool_calls {
        // Persist the assistant message (with tool_calls) before awaiting tools.
        {
            let mut store = config.message_store.lock().await;
            store.push(assistant_message.clone()).await;
        }

        for tc in tool_calls {
            let args = tc.function.arguments.clone().unwrap_or_default();
            let args_value = serde_json::from_str(&args).unwrap_or(Value::Null);

            emit(
                &tx,
                BrainEvent::ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: args_value.clone(),
                },
            );

            config
                .hook_policy
                .on_pre_tool_use(&tc.function.name, &args_value, &tc.id)
                .await;
        }

        let results = step_result.tool_results().await;

        for result in &results {
            let transformed = if let Some(transformer) = &config.tool_result_transformer {
                transformer
                    .transform(
                        &result.tool_call_id,
                        "", // tool name is not directly available on KosongToolResult
                        result.return_value.clone(),
                    )
                    .await?
            } else {
                result.return_value.clone()
            };

            let output = transformed
                .output
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message = transformed
                .message
                .clone()
                .unwrap_or_else(|| output.clone());
            let is_error = transformed.is_error;

            emit(
                &tx,
                BrainEvent::ToolResult {
                    id: result.tool_call_id.clone(),
                    output: message.clone(),
                    is_error,
                },
            );

            append_tool_result(&config.message_store, &result.tool_call_id, transformed).await;

            if is_error {
                config
                    .hook_policy
                    .on_post_tool_use_failure("", &Value::Null, &message, &result.tool_call_id)
                    .await;
            } else {
                config
                    .hook_policy
                    .on_post_tool_use("", &Value::Null, &message, &result.tool_call_id)
                    .await;
            }
        }

        if let Some(policy) = &config.step_policy {
            return policy
                .after_step(ctx, &StepOutcome::Continue, &results)
                .await;
        }
        return Ok(StepControl::Continue);
    }

    // No tool calls: this is the final answer for the step.
    {
        let mut store = config.message_store.lock().await;
        store.push(assistant_message).await;
    }

    if let Some(policy) = &config.step_policy {
        return policy
            .after_step(
                ctx,
                &StepOutcome::Final {
                    text: assistant_text.clone(),
                },
                &[],
            )
            .await;
    }
    Ok(StepControl::Stop {
        final_text: assistant_text,
    })
}

async fn append_tool_result(
    message_store: &Arc<tokio::sync::Mutex<dyn crate::session::store::MessageStore>>,
    tool_call_id: &str,
    return_value: ToolReturnValue,
) {
    let message = return_value
        .message
        .clone()
        .or_else(|| {
            return_value
                .output
                .as_ref()
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();

    let mut store = message_store.lock().await;
    store
        .push(Message {
            role: Role::Tool,
            name: None,
            content: vec![ContentPart::Text { text: message }],
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            partial: None,
        })
        .await;
}

/// Toolset wrapper that gates each tool call through an [`ApprovalRuntime`].
#[derive(Clone)]
struct ApprovalToolset {
    inner: ToolRegistry,
    runtime: Arc<dyn ApprovalRuntime>,
    event_tx: UnboundedSender<BrainEvent>,
}

impl llm_provider::Toolset for ApprovalToolset {
    fn tools(&self) -> Vec<llm_provider::Tool> {
        self.inner.tools()
    }

    fn handle(&self, tool_call: &llm_provider::ToolCall) -> HandleResult {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let event_tx = self.event_tx.clone();
        let tc = tool_call.clone();

        let handle = tokio::spawn(async move {
            let args_value = tc
                .function
                .arguments
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);

            let request = ApprovalRequest {
                tool_name: tc.function.name.clone(),
                tool_input: args_value,
                tool_call_id: tc.id.clone(),
                display: Vec::new(),
            };

            let response = runtime.request(request, event_tx.clone()).await;
            let approved = response.is_approved();

            if approved {
                match inner.handle(&tc) {
                    HandleResult::Ready(result) => result,
                    HandleResult::Pending(handle) => match handle.await {
                        Ok(result) => result,
                        Err(e) => llm_provider::tooling::ToolResult {
                            tool_call_id: tc.id,
                            return_value: llm_provider::tooling::ToolReturnValue::error(format!(
                                "Tool execution failed: {}",
                                e
                            )),
                        },
                    },
                }
            } else {
                let reason = match response {
                    ApprovalResponse::Approved => None,
                    ApprovalResponse::Rejected { feedback } => Some(feedback),
                };
                llm_provider::tooling::ToolResult {
                    tool_call_id: tc.id,
                    return_value: llm_provider::tooling::ToolReturnValue::error(
                        reason.unwrap_or_else(|| "Tool call rejected".to_string()),
                    ),
                }
            }
        });

        HandleResult::Pending(handle)
    }
}

/// Toolset wrapper that enforces the [`HookPolicy`] before each tool call.
#[derive(Clone)]
struct HookAwareToolset {
    inner: Arc<dyn llm_provider::Toolset>,
    hook_policy: Arc<dyn crate::hooks::policy::HookPolicy>,
}

impl llm_provider::Toolset for HookAwareToolset {
    fn tools(&self) -> Vec<llm_provider::Tool> {
        self.inner.tools()
    }

    fn handle(&self, tool_call: &llm_provider::ToolCall) -> HandleResult {
        let args_value = tool_call
            .function
            .arguments
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        let hook_policy = self.hook_policy.clone();
        let inner = self.inner.clone();
        let tc = tool_call.clone();

        let handle = tokio::spawn(async move {
            match hook_policy
                .on_pre_tool_use(&tc.function.name, &args_value, &tc.id)
                .await
            {
                crate::hooks::policy::HookAction::Allow => match inner.handle(&tc) {
                    HandleResult::Ready(result) => result,
                    HandleResult::Pending(handle) => match handle.await {
                        Ok(result) => result,
                        Err(e) => llm_provider::tooling::ToolResult {
                            tool_call_id: tc.id,
                            return_value: llm_provider::tooling::ToolReturnValue::error(format!(
                                "Tool execution failed: {}",
                                e
                            )),
                        },
                    },
                },
                crate::hooks::policy::HookAction::Block { reason } => {
                    llm_provider::tooling::ToolResult {
                        tool_call_id: tc.id,
                        return_value: llm_provider::tooling::ToolReturnValue::error(reason),
                    }
                }
            }
        });

        HandleResult::Pending(handle)
    }
}
