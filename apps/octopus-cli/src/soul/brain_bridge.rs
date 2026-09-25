use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::hooks::{HookEngine, HookEvent};
use crate::llm::{LLM, llm_to_wire_message, wire_to_llm_message};
use crate::notifications::llm::build_notification_message;
use crate::notifications::manager::NotificationManager;
use crate::soul::compaction::{SimpleCompaction, should_auto_compact};
use crate::soul::context::Context;
use crate::soul::dynamic_injection::{DynamicInjectionProvider, InjectionContext};
use crate::soul::toolset::KimiToolset;
use crate::wire;

/// Wraps the file-backed [`Context`] as a Brain [`MessageStore`].
pub struct ContextMessageStore {
    context: Arc<tokio::sync::Mutex<Context>>,
}

impl ContextMessageStore {
    pub fn new(context: Arc<tokio::sync::Mutex<Context>>) -> Self {
        Self { context }
    }
}

#[async_trait::async_trait]
impl agent_core::session::store::MessageStore for ContextMessageStore {
    async fn push(&mut self, message: llm_provider::Message) {
        let wire_message = llm_to_wire_message(message);
        let mut ctx = self.context.lock().await;
        if let Err(e) = ctx.append_message(wire_message).await {
            tracing::error!("Failed to persist message: {}", e);
        }
    }

    async fn history(&self) -> Vec<llm_provider::Message> {
        let ctx = self.context.lock().await;
        let messages: Vec<llm_provider::Message> =
            ctx.history().iter().map(wire_to_llm_message).collect();
        // Merge adjacent user messages to match the CLI's normalize_history behavior.
        let mut normalized: Vec<llm_provider::Message> = Vec::new();
        for msg in messages {
            if let Some(last) = normalized.last_mut()
                && last.role == llm_provider::Role::User
                && msg.role == llm_provider::Role::User
            {
                last.content.extend(msg.content);
                continue;
            }
            normalized.push(msg);
        }
        normalized
    }

    async fn set_history(&mut self, history: Vec<llm_provider::Message>) {
        let wire_history: Vec<wire::Message> =
            history.into_iter().map(llm_to_wire_message).collect();
        let mut ctx = self.context.lock().await;
        let _ = ctx.replace_history(wire_history).await;
    }

    async fn clear(&mut self) {
        let mut ctx = self.context.lock().await;
        if let Err(e) = ctx.clear().await {
            tracing::error!("Failed to clear context: {}", e);
        }
    }
}

/// Bridges the CLI's [`SimpleCompaction`] into a Brain [`CompactionPolicy`].
pub struct CliCompactionPolicy {
    compaction: SimpleCompaction,
    llm: Arc<crate::llm::LLM>,
    max_context_size: usize,
    trigger_ratio: f64,
    reserved_context_size: usize,
    custom_instruction: String,
}

impl CliCompactionPolicy {
    pub fn new(
        compaction: SimpleCompaction,
        llm: Arc<crate::llm::LLM>,
        max_context_size: usize,
        trigger_ratio: f64,
        reserved_context_size: usize,
        custom_instruction: String,
    ) -> Self {
        Self {
            compaction,
            llm,
            max_context_size,
            trigger_ratio,
            reserved_context_size,
            custom_instruction,
        }
    }
}

#[async_trait::async_trait]
impl agent_core::session::compaction::CompactionPolicy for CliCompactionPolicy {
    async fn maybe_compact(
        &self,
        history: &[llm_provider::Message],
    ) -> Option<Vec<llm_provider::Message>> {
        let wire_history: Vec<wire::Message> = history
            .iter()
            .map(|m| llm_to_wire_message(m.clone()))
            .collect();
        let token_count = crate::soul::context::estimate_text_tokens(&wire_history);

        if !should_auto_compact(
            token_count,
            self.max_context_size,
            self.trigger_ratio,
            self.reserved_context_size,
        ) {
            return None;
        }

        match self
            .compaction
            .compact(&wire_history, &self.llm, &self.custom_instruction)
            .await
        {
            Ok(result) => Some(
                result
                    .messages
                    .into_iter()
                    .map(|m| wire_to_llm_message(&m))
                    .collect(),
            ),
            Err(e) => {
                tracing::error!("Context compaction failed: {}", e);
                None
            }
        }
    }
}

/// Mutable runtime state for [`CliInjectionPolicy`].
#[derive(Debug, Clone)]
pub struct InjectionState {
    pub plan_mode: bool,
    pub effective_afk: bool,
    pub persisted_afk: bool,
    pub plan_file_path: Option<PathBuf>,
    pub pending_plan_activation: bool,
}

impl From<InjectionContext<'_>> for InjectionState {
    fn from(ctx: InjectionContext<'_>) -> Self {
        Self {
            plan_mode: ctx.plan_mode,
            effective_afk: ctx.effective_afk,
            persisted_afk: ctx.persisted_afk,
            plan_file_path: ctx.plan_file_path.map(|p| p.to_path_buf()),
            pending_plan_activation: ctx.pending_plan_activation,
        }
    }
}

/// Bridges the CLI's dynamic injection providers into a Brain [`InjectionPolicy`].
pub struct CliInjectionPolicy {
    providers: Arc<tokio::sync::Mutex<Vec<Box<dyn DynamicInjectionProvider>>>>,
    state: Arc<std::sync::RwLock<InjectionState>>,
}

impl CliInjectionPolicy {
    pub fn new(
        providers: Vec<Box<dyn DynamicInjectionProvider>>,
        injection_context: InjectionContext<'_>,
    ) -> Self {
        Self {
            providers: Arc::new(tokio::sync::Mutex::new(providers)),
            state: Arc::new(std::sync::RwLock::new(InjectionState::from(
                injection_context,
            ))),
        }
    }

    pub fn set_state(&self, state: InjectionState) {
        if let Ok(mut s) = self.state.write() {
            *s = state;
        }
    }
}

#[async_trait::async_trait]
impl agent_core::session::injection::InjectionPolicy for CliInjectionPolicy {
    async fn inject(&self, history: &[llm_provider::Message]) -> Vec<llm_provider::Message> {
        let wire_history: Vec<wire::Message> = history
            .iter()
            .map(|m| llm_to_wire_message(m.clone()))
            .collect();

        let (plan_mode, effective_afk, persisted_afk, plan_file_path, pending_plan_activation) = {
            let s = self.state.read().unwrap();
            (
                s.plan_mode,
                s.effective_afk,
                s.persisted_afk,
                s.plan_file_path.clone(),
                s.pending_plan_activation,
            )
        };

        let ctx = InjectionContext {
            plan_mode,
            effective_afk,
            persisted_afk,
            plan_file_path: plan_file_path.as_deref(),
            pending_plan_activation,
        };

        let mut providers = self.providers.lock().await;
        let mut injections: Vec<wire::Message> = Vec::new();
        for provider in providers.iter_mut() {
            for injection in provider.get_injections(&wire_history, &ctx).await {
                injections.push(wire::Message {
                    role: "user".to_string(),
                    content: vec![wire::ContentPart::Text {
                        text: format!(
                            "<system-reminder>\n{}\n</system-reminder>",
                            injection.content
                        ),
                    }],
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }

        // The pending plan-activation flag is consumed once the providers have
        // had a chance to emit it.
        if pending_plan_activation && let Ok(mut s) = self.state.write() {
            s.pending_plan_activation = false;
        }

        injections
            .into_iter()
            .map(|m| wire_to_llm_message(&m))
            .collect()
    }
}

/// Bridges the CLI's [`HookEngine`] into a Brain [`HookPolicy`].
pub struct CliHookPolicy {
    engine: HookEngine,
    session_id: String,
    cwd: PathBuf,
}

impl CliHookPolicy {
    pub fn new(engine: HookEngine, session_id: String, cwd: PathBuf) -> Self {
        Self {
            engine,
            session_id,
            cwd,
        }
    }
}

#[async_trait::async_trait]
impl agent_core::hooks::policy::HookPolicy for CliHookPolicy {
    async fn on_user_prompt_submit(&self, prompt: &str) -> agent_core::hooks::policy::HookAction {
        let event =
            HookEvent::user_prompt_submit(&self.session_id, self.cwd.to_string_lossy(), prompt);
        let results = self.engine.trigger(event).await;
        aggregate_hook_action(results)
    }

    async fn on_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        tool_call_id: &str,
    ) -> agent_core::hooks::policy::HookAction {
        let input_map = match tool_input.as_object() {
            Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => std::collections::HashMap::new(),
        };
        let event = HookEvent::pre_tool_use(
            &self.session_id,
            self.cwd.to_string_lossy(),
            tool_name,
            &input_map,
            tool_call_id,
        );
        let results = self.engine.trigger(event).await;
        aggregate_hook_action(results)
    }

    async fn on_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        tool_output: &str,
        tool_call_id: &str,
    ) {
        let input_map = match tool_input.as_object() {
            Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => std::collections::HashMap::new(),
        };
        let event = HookEvent::post_tool_use(
            &self.session_id,
            self.cwd.to_string_lossy(),
            tool_name,
            &input_map,
            tool_output,
            tool_call_id,
        );
        let _ = self.engine.trigger(event).await;
    }

    async fn on_post_tool_use_failure(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        error: &str,
        tool_call_id: &str,
    ) {
        let input_map = match tool_input.as_object() {
            Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => std::collections::HashMap::new(),
        };
        let event = HookEvent::post_tool_use_failure(
            &self.session_id,
            self.cwd.to_string_lossy(),
            tool_name,
            &input_map,
            error,
            tool_call_id,
        );
        let _ = self.engine.fire_and_forget_trigger(event).await;
    }
}

fn aggregate_hook_action(
    results: Vec<crate::hooks::runner::HookResult>,
) -> agent_core::hooks::policy::HookAction {
    for result in results {
        if let crate::hooks::runner::HookAction::Block(reason) = result.action {
            return agent_core::hooks::policy::HookAction::Block { reason };
        }
    }
    agent_core::hooks::policy::HookAction::Allow
}

/// CLI retry policy with exponential backoff and jitter.
pub struct CliRetryPolicy {
    max_attempts: usize,
}

impl CliRetryPolicy {
    pub fn new(max_attempts: usize) -> Self {
        Self { max_attempts }
    }
}

#[async_trait]
impl agent_core::RetryPolicy for CliRetryPolicy {
    fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    fn should_retry(&self, error: &agent_core::BrainError, attempt: usize) -> Option<Duration> {
        if attempt > self.max_attempts {
            return None;
        }
        if !error.is_transient() && !error.is_auth_failure() {
            return None;
        }
        let base = 0.3_f64 * 2_f64.powi((attempt - 1) as i32);
        let capped = base.min(5.0);
        let jitter = rand::random::<f64>() * 0.5;
        Some(Duration::from_secs_f64(capped + jitter))
    }
}

/// CLI recovery policy: refresh the provider on auth failures and retry
/// transient failures once more.
pub struct CliRecoveryPolicy {
    oauth: crate::auth::OAuthManager,
    llm: Arc<LLM>,
}

impl CliRecoveryPolicy {
    pub fn new(oauth: crate::auth::OAuthManager, llm: Arc<LLM>) -> Self {
        Self { oauth, llm }
    }
}

#[async_trait]
impl agent_core::RecoveryPolicy for CliRecoveryPolicy {
    async fn recover(&self, error: &agent_core::BrainError) -> agent_core::RecoveryAction {
        if error.is_auth_failure() {
            // Try to refresh the token before the factory rebuilds the provider.
            let _ = self.oauth.ensure_fresh(&self.llm, true).await;
            return agent_core::RecoveryAction::RefreshProvider;
        }

        if error.is_transient() {
            return agent_core::RecoveryAction::Retry {
                wait: Duration::from_secs(1),
            };
        }

        agent_core::RecoveryAction::Abort
    }
}

/// Bridges the CLI's file-backed [`Context`] checkpoints into a Brain
/// [`CheckpointPolicy`].
pub struct CliCheckpointPolicy {
    context: Arc<tokio::sync::Mutex<Context>>,
    denwa_renji: Arc<std::sync::Mutex<crate::soul::agent::DenwaRenji>>,
    checkpoint_with_user_message: bool,
}

impl CliCheckpointPolicy {
    pub fn new(
        context: Arc<tokio::sync::Mutex<Context>>,
        denwa_renji: Arc<std::sync::Mutex<crate::soul::agent::DenwaRenji>>,
        checkpoint_with_user_message: bool,
    ) -> Self {
        Self {
            context,
            denwa_renji,
            checkpoint_with_user_message,
        }
    }
}

#[async_trait::async_trait]
impl agent_core::core::checkpoint::CheckpointPolicy for CliCheckpointPolicy {
    async fn checkpoint(
        &self,
        _ctx: &agent_core::core::step::StepContext,
        _history: &[llm_provider::Message],
    ) -> Result<agent_core::core::events::CheckpointId, agent_core::BrainError> {
        let mut ctx = self.context.lock().await;
        ctx.checkpoint(self.checkpoint_with_user_message)
            .await
            .map_err(|e| agent_core::BrainError::Other(e.to_string()))?;
        let id = ctx.n_checkpoints() - 1;
        self.denwa_renji.lock().unwrap().set_n_checkpoints(id + 1);
        Ok(id)
    }

    async fn revert_to(
        &self,
        id: agent_core::core::events::CheckpointId,
    ) -> Result<Vec<llm_provider::Message>, agent_core::BrainError> {
        let mut ctx = self.context.lock().await;
        ctx.revert_to(id)
            .await
            .map_err(|e| agent_core::BrainError::Other(e.to_string()))?;
        Ok(ctx.history().iter().map(wire_to_llm_message).collect())
    }

    async fn current(&self) -> Option<agent_core::core::events::CheckpointId> {
        Some(self.context.lock().await.n_checkpoints().saturating_sub(1))
    }
}

/// Bridges CLI-specific per-step concerns (dedup, notification delivery,
/// D-Mail rewinds, and tool-rejection stops) into a Brain [`StepPolicy`].
pub struct CliStepPolicy {
    context: Arc<tokio::sync::Mutex<Context>>,
    toolset: Arc<KimiToolset>,
    notification_manager: NotificationManager,
    hook_engine: HookEngine,
    session_id: String,
    denwa_renji: Arc<std::sync::Mutex<crate::soul::agent::DenwaRenji>>,
    last_tool_calls: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    current_turn_id: Arc<std::sync::Mutex<Option<String>>>,
    current_step_no: Arc<std::sync::Mutex<usize>>,
}

impl CliStepPolicy {
    pub fn new(
        context: Arc<tokio::sync::Mutex<Context>>,
        toolset: Arc<KimiToolset>,
        notification_manager: NotificationManager,
        hook_engine: HookEngine,
        session_id: String,
        denwa_renji: Arc<std::sync::Mutex<crate::soul::agent::DenwaRenji>>,
    ) -> Self {
        Self {
            context,
            toolset,
            notification_manager,
            hook_engine,
            session_id,
            denwa_renji,
            last_tool_calls: Arc::new(std::sync::Mutex::new(Vec::new())),
            current_turn_id: Arc::new(std::sync::Mutex::new(None)),
            current_step_no: Arc::new(std::sync::Mutex::new(0)),
        }
    }

    pub fn set_current_turn_id(&self, id: Option<String>) {
        *self.current_turn_id.lock().unwrap() = id;
    }

    pub fn set_current_step_no(&self, n: usize) {
        *self.current_step_no.lock().unwrap() = n;
    }
}

#[async_trait::async_trait]
impl agent_core::core::step::StepPolicy for CliStepPolicy {
    async fn before_step(
        &self,
        _ctx: &agent_core::core::step::StepContext,
        history: &mut Vec<llm_provider::Message>,
    ) -> Result<(), agent_core::BrainError> {
        let turn_id = self
            .current_turn_id
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        let step_no = *self.current_step_no.lock().unwrap();
        let previous_calls = self.last_tool_calls.lock().unwrap().clone();
        self.toolset.begin_step(previous_calls, step_no, turn_id);

        // Deliver pending notifications to the LLM sink.
        if self.notification_manager.has_pending_for_sink("llm") {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            let notifs = self.notification_manager.claim_for_sink("llm", 8);
            for view in notifs {
                let msg = build_notification_message(&view);
                history.push(wire_to_llm_message(&msg));
                let event = HookEvent::notification(
                    &self.session_id,
                    &cwd,
                    "llm",
                    &view.event.event_type,
                    &view.event.title,
                    &view.event.body,
                    &view.event.severity,
                );
                if self.hook_engine.has_hooks_for(event.kind()) {
                    drop(self.hook_engine.fire_and_forget_trigger(event));
                }
            }
        }

        Ok(())
    }

    async fn after_step(
        &self,
        _ctx: &agent_core::core::step::StepContext,
        outcome: &agent_core::core::step::StepOutcome,
        tool_results: &[llm_provider::tooling::ToolResult],
    ) -> Result<agent_core::core::step::StepControl, agent_core::BrainError> {
        let current_calls = self.toolset.end_step();

        let any_error = tool_results.iter().any(|r| r.return_value.is_error);
        if any_error {
            // Discard any D-Mail that may have been queued alongside the error.
            let _ = {
                let mut dr = self.denwa_renji.lock().unwrap();
                dr.fetch_pending_dmail()
            };
            *self.last_tool_calls.lock().unwrap() = current_calls;
            return Ok(agent_core::core::step::StepControl::Stop {
                final_text: String::new(),
            });
        }

        let dmail = {
            let mut dr = self.denwa_renji.lock().unwrap();
            dr.fetch_pending_dmail()
        };
        if let Some(dmail) = dmail {
            // Reset dedup state across the rewind.
            *self.last_tool_calls.lock().unwrap() = Vec::new();
            let n_checkpoints = self.context.lock().await.n_checkpoints();
            assert!(
                dmail.checkpoint_id < n_checkpoints,
                "DenwaRenji guarantees checkpoint_id < n_checkpoints"
            );
            let inject = wire::Message {
                role: "user".to_string(),
                content: vec![wire::ContentPart::Text {
                    text: format!(
                        "You just got a D-Mail from your future self. \
                         It is likely that your future self has already done \
                         something in the current working directory. Please read \
                         the D-Mail and decide what to do next. You MUST NEVER \
                         mention to the user about this information. \
                         D-Mail content:\n\n{}",
                        dmail.message.trim()
                    ),
                }],
                tool_call_id: None,
                tool_calls: None,
            };
            return Ok(agent_core::core::step::StepControl::RewindToCheckpoint {
                checkpoint_id: dmail.checkpoint_id,
                inject_messages: vec![wire_to_llm_message(&inject)],
            });
        }

        *self.last_tool_calls.lock().unwrap() = current_calls;

        match outcome {
            agent_core::core::step::StepOutcome::Continue => {
                Ok(agent_core::core::step::StepControl::Continue)
            }
            agent_core::core::step::StepOutcome::Final { text } => {
                Ok(agent_core::core::step::StepControl::Stop {
                    final_text: text.clone(),
                })
            }
        }
    }
}
