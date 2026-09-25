use std::path::PathBuf;
use std::sync::Arc;

use crate::approval_runtime::{ApprovalRuntime, ApprovalSource};
use crate::auth::OAuthManager;
use crate::config::Config;
use crate::exception::{LLMNotSet, LLMNotSupported, MaxStepsReached, OctopusError, Result};
use crate::hooks::{HookEngine, HookEvent};
use crate::llm::{LLM, llm_to_wire_usage};
use crate::notifications::llm::extract_notification_ids;
use crate::notifications::manager::NotificationManager;
use crate::session::Session;
use crate::soul::approval::{Approval, ApprovalState};
use crate::soul::brain_bridge::{
    CliCheckpointPolicy, CliCompactionPolicy, CliInjectionPolicy, CliRecoveryPolicy,
    CliRetryPolicy, CliStepPolicy, ContextMessageStore,
};
use crate::soul::compaction::SimpleCompaction;
use crate::soul::context::Context;
use crate::soul::dynamic_injection::{DynamicInjectionProvider, InjectionContext};
use crate::soul::message::check_message;
use crate::soul::slash::{build_default_slash_commands, parse_slash_command_call};
use crate::soul::toolset::KimiToolset;
use crate::wire::{
    CompactionBegin, CompactionEnd, ContentPart, Message, RootWireHub, StatusUpdate, SteerInput,
    StepBegin, StepInterrupted, TextPart, ToolResult, TurnBegin, TurnEnd, wire_send,
};
use futures::StreamExt;

pub struct KimiSoul {
    pub config: Config,
    pub session: Session,
    pub llm: Option<LLM>,
    pub context: Arc<tokio::sync::Mutex<Context>>,
    pub toolset: std::sync::Arc<KimiToolset>,
    pub approval: Approval,
    pub plan_mode: bool,
    pub max_steps_per_turn: usize,
    pub max_retries_per_step: usize,
    pub agent: crate::soul::agent::Agent,
    pub slash_registry: crate::soul::slash::SlashCommandRegistry,
    pub root_wire_hub: Option<RootWireHub>,
    pub hook_engine: HookEngine,
    pub notification_manager: NotificationManager,
    pub oauth: OAuthManager,
    pub denwa_renji: std::sync::Arc<std::sync::Mutex<crate::soul::agent::DenwaRenji>>,
    pub skills: crate::skills::SkillRegistry,
    pub bg_manager: crate::background::BackgroundTaskManager,
    pub brain: Option<agent_core::Brain>,
    injection_policy: Arc<CliInjectionPolicy>,
    step_policy: Arc<CliStepPolicy>,
    checkpoint_policy: Arc<CliCheckpointPolicy>,
    steer_queue: Vec<String>,
    compaction: SimpleCompaction,
    current_step_no: usize,
    current_turn_id: Option<String>,
    injection_providers: Vec<Box<dyn DynamicInjectionProvider>>,
    pending_plan_activation_injection: bool,
    plan_session_id: Option<String>,
}

impl KimiSoul {
    pub fn new(
        config: Config,
        session: Session,
        llm: Option<LLM>,
        approval_state: ApprovalState,
        mut agent: crate::soul::agent::Agent,
        tool_policy: Option<crate::subagents::ToolPolicy>,
    ) -> Self {
        let mut approval = Approval::with_state(approval_state.clone());
        let approval_runtime = ApprovalRuntime::new();
        let root_wire_hub = RootWireHub::new();
        approval_runtime.bind_root_wire_hub(&root_wire_hub);
        approval.set_runtime(approval_runtime);

        let context_file = session.context_file.clone();
        let mut context = Context::new(context_file);
        let _ = context.restore_sync();

        // Ack any notification IDs already present in restored context.
        let ack_ids = extract_notification_ids(context.history());

        let context = Arc::new(tokio::sync::Mutex::new(context));

        let bg_manager = agent.runtime.background_tasks.clone();
        let denwa_renji =
            std::sync::Arc::new(std::sync::Mutex::new(agent.runtime.denwa_renji.clone()));

        let mut toolset = std::mem::take(&mut agent.toolset);
        // Ensure core tools are present even if the agent spec omitted them.
        let has_shell = toolset.find("Shell").is_some();
        let has_task_output = toolset.find("TaskOutput").is_some();
        let has_task_stop = toolset.find("TaskStop").is_some();
        let has_agent = toolset.find("Agent").is_some();
        let has_dmail = toolset.find("SendDMail").is_some();
        if !has_shell {
            toolset.register_typed(crate::tools::shell::ShellTool::new(bg_manager.clone()));
        }
        if !has_task_output {
            toolset.register_typed(crate::tools::background::TaskOutputTool::new(
                bg_manager.clone(),
            ));
        }
        if !has_task_stop {
            toolset.register_typed(crate::tools::background::TaskStopTool::new(
                bg_manager.clone(),
            ));
        }
        if !has_agent {
            toolset.register_typed(crate::tools::agent::AgentTool::new(agent.runtime.clone()));
        }
        if !has_dmail {
            toolset.register_typed(crate::tools::dmail::SendDMailTool::new(denwa_renji.clone()));
        }

        // Enforce tool policy for subagents.
        if let Some(policy) = tool_policy {
            match policy {
                crate::subagents::ToolPolicy::Inherit => {}
                crate::subagents::ToolPolicy::AllowList { tools } => {
                    toolset.hide_all_except(&tools);
                }
            }
        }

        let checkpoint_with_user_message = toolset.tools().iter().any(|t| t.name() == "SendDMail");

        let max_steps = config.loop_control.max_steps_per_turn;
        let max_retries = config.loop_control.max_retries_per_step;

        let plan_mode = session.state.plan_mode;
        let plan_session_id = session.state.plan_session_id.clone();
        let session_id = session.id.clone();
        let hooks = config.hooks.clone();

        let hook_engine = HookEngine::new(hooks)
            .with_cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        toolset.set_hook_engine(hook_engine.clone());
        toolset.set_session_id(session_id.clone());
        toolset.set_approval(Some(approval.share()));
        let toolset = std::sync::Arc::new(toolset);

        // Discover skills
        let mut skills = crate::skills::SkillRegistry::new();
        let mut skill_dirs = Vec::new();
        // User skills
        let user_skills_dir = dirs::home_dir()
            .map(|h| h.join(".kimi").join("skills"))
            .filter(|p| p.is_dir());
        if let Some(dir) = user_skills_dir {
            skill_dirs.push(dir);
        }
        // Project skills
        let project_skills_dir = session.work_dir.join(".kimi").join("skills");
        if project_skills_dir.is_dir() {
            skill_dirs.push(project_skills_dir);
        }
        // Extra skill dirs from config
        for dir in &config.extra_skill_dirs {
            let path = PathBuf::from(dir);
            if path.is_dir() {
                skill_dirs.push(path);
            }
        }
        if config.merge_all_available_skills {
            skills.discover(&skill_dirs);
        }
        tracing::info!("Discovered {} skills", skills.len());

        let notification_root = crate::share::get_share_dir()
            .join("notifications")
            .join(&session_id);
        let notification_manager =
            NotificationManager::new(notification_root, config.notifications.clone());

        if !ack_ids.is_empty() {
            notification_manager.ack_ids("llm", &ack_ids);
        }

        let oauth = OAuthManager::new();

        // Dynamic injection state is shared with Brain and updated when toggles change.
        let plan_file_path = plan_session_id.as_ref().map(|id| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".kimi")
                .join("plans")
                .join(format!("{id}.md"))
        });
        let injection_context = InjectionContext {
            plan_mode,
            effective_afk: approval.is_afk(),
            persisted_afk: approval.state().mode.is_afk(),
            plan_file_path: plan_file_path.as_deref(),
            pending_plan_activation: false,
        };
        let injection_policy = Arc::new(CliInjectionPolicy::new(
            vec![
                Box::new(
                    crate::soul::dynamic_injections::plan_mode::PlanModeInjectionProvider::new(),
                ),
                Box::new(
                    crate::soul::dynamic_injections::afk_mode::AfkModeInjectionProvider::new(),
                ),
            ],
            injection_context,
        ));

        // CLI-specific Brain policies. Brain itself is built lazily on the first
        // turn so that MCP loading can mutate the toolset while it is still the
        // sole owner of the underlying Arc.
        let step_policy = Arc::new(CliStepPolicy::new(
            context.clone(),
            toolset.clone(),
            notification_manager.clone(),
            hook_engine.clone(),
            session_id.clone(),
            denwa_renji.clone(),
        ));
        let checkpoint_policy = Arc::new(CliCheckpointPolicy::new(
            context.clone(),
            denwa_renji.clone(),
            checkpoint_with_user_message,
        ));

        Self {
            config,
            session,
            llm,
            context,
            toolset: toolset.clone(),
            approval,
            plan_mode,
            max_steps_per_turn: max_steps,
            max_retries_per_step: max_retries,
            agent,
            slash_registry: build_default_slash_commands(),
            root_wire_hub: Some(root_wire_hub),
            hook_engine,
            notification_manager,
            oauth,
            denwa_renji,
            skills,
            bg_manager,
            brain: None,
            injection_policy,
            step_policy,
            checkpoint_policy,
            steer_queue: Vec::new(),
            compaction: SimpleCompaction::default(),
            current_step_no: 0,
            current_turn_id: None,
            injection_providers: vec![
                Box::new(
                    crate::soul::dynamic_injections::plan_mode::PlanModeInjectionProvider::new(),
                ),
                Box::new(
                    crate::soul::dynamic_injections::afk_mode::AfkModeInjectionProvider::new(),
                ),
            ],
            pending_plan_activation_injection: false,
            plan_session_id,
        }
    }

    pub async fn shutdown(&mut self) {
        self.bg_manager.shutdown().await;
    }

    pub async fn run(&mut self, user_input: &str) -> Result<String> {
        let text = user_input.trim();
        if text.is_empty() {
            return Ok(String::new());
        }

        // Set up wire channel for this run.
        let wire_file = crate::wire::file::WireFile::new(self.session.wire_file_path.clone());
        let wire = crate::wire::channel::Wire::new(Some(wire_file));
        let result = self.run_with_wire(text, &wire, None).await;

        // Dropping `wire` drops the broadcast senders, which causes the
        // recorder task to exit cleanly after flushing.
        drop(wire);

        result
    }

    /// Run a single turn using the provided wire channel.
    ///
    /// The caller retains ownership of the `Wire` and is responsible for
    /// reading from `wire.ui_side()` to consume events sent via `wire_send()`.
    pub async fn run_with_wire(
        &mut self,
        user_input: &str,
        wire: &crate::wire::channel::Wire,
        on_wire_hook: Option<crate::hooks::OnWireHook>,
    ) -> Result<String> {
        let text = user_input.trim();
        if text.is_empty() {
            return Ok(String::new());
        }

        let soul_side = wire.soul_side();

        crate::wire::with_wire_soul_side(Some(soul_side.clone()), async {
            // Start notification pump — delivers pending notifications to wire.
            let pump_handle = self.start_notification_pump(soul_side.clone());

            // Wire hook callbacks: emit HookTriggered / HookResolved events.
            // Wire hook dispatch is stubbed (no request/response protocol yet)
            // so client-side hooks always fail open.
            self.hook_engine.set_callbacks(crate::hooks::HookCallbacks {
                on_triggered: Some(std::sync::Arc::new(
                    |event: &crate::hooks::HookEvent, target: &str, count: usize| {
                        crate::wire::wire_send(crate::wire::WireEvent::HookTriggered(
                            crate::wire::HookTriggered {
                                event: event.kind().to_string(),
                                target: target.to_string(),
                                hook_count: count,
                            },
                        ));
                    },
                )),
                on_resolved: Some(std::sync::Arc::new(
                    |event: &crate::hooks::HookEvent,
                     target: &str,
                     action: crate::hooks::runner::HookAction,
                     duration_ms: u64| {
                        let (action_str, reason) = match action {
                            crate::hooks::runner::HookAction::Allow => {
                                ("allow".to_string(), String::new())
                            }
                            crate::hooks::runner::HookAction::Block(ref r) => {
                                ("block".to_string(), r.clone())
                            }
                        };
                        crate::wire::wire_send(crate::wire::WireEvent::HookResolved(
                            crate::wire::HookResolved {
                                event: event.kind().to_string(),
                                target: target.to_string(),
                                action: action_str,
                                reason,
                                duration_ms,
                            },
                        ));
                    },
                )),
                on_wire_hook: on_wire_hook.or_else(|| {
                    Some(std::sync::Arc::new(
                        |handle: crate::hooks::WireHookHandle| {
                            // No wire request/response protocol yet — fail open.
                            handle.resolve(crate::hooks::runner::HookAction::Allow);
                            Box::pin(async {})
                        },
                    ))
                }),
                on_wire_hook_done: None,
            });

            let result = self.run_turn(text).await;

            // Cleanup
            if let Some(handle) = pump_handle {
                handle.abort();
            }
            result
        })
        .await
    }

    async fn run_turn(&mut self, text: &str) -> Result<String> {
        // Inherit an existing approval source (e.g., from a subagent or background task),
        // or create a new foreground-turn source if none exists.
        let (source, inherited) = if let Some(existing) =
            crate::approval_runtime::get_current_approval_source_or_none()
        {
            (existing, true)
        } else {
            (
                ApprovalSource {
                    kind: crate::approval_runtime::ApprovalSourceKind::ForegroundTurn,
                    id: uuid::Uuid::new_v4().to_string(),
                    agent_id: None,
                },
                false,
            )
        };

        let result = if inherited {
            self.run_turn_body(text).await
        } else {
            crate::approval_runtime::with_approval_source(source.clone(), self.run_turn_body(text))
                .await
        };

        // Only cancel approvals if WE created the source for this turn.
        if !inherited {
            self.approval
                .runtime()
                .cancel_by_source(source.kind, &source.id);
        }

        result
    }

    async fn ensure_brain(&mut self) -> Result<&mut agent_core::Brain> {
        if self.brain.is_none() {
            self.init_brain()?;
        }
        Ok(self.brain.as_mut().unwrap())
    }

    fn init_brain(&mut self) -> Result<()> {
        let llm = self
            .llm
            .as_ref()
            .ok_or(crate::exception::LLMNotSet::NotSet)?;
        let message_store = Arc::new(tokio::sync::Mutex::new(ContextMessageStore::new(
            self.context.clone(),
        )));
        let compaction_policy = Arc::new(CliCompactionPolicy::new(
            SimpleCompaction::new(2),
            Arc::new(llm.clone()),
            llm.max_context_size,
            self.config.loop_control.compaction_trigger_ratio,
            self.config.loop_control.reserved_context_size,
            String::new(),
        ));
        let hook_policy = Arc::new(agent_core::hooks::policy::NoOpHookPolicy);
        let retry_policy = Arc::new(CliRetryPolicy::new(self.max_retries_per_step));
        let recovery_policy = Arc::new(CliRecoveryPolicy::new(
            self.oauth.clone(),
            Arc::new(llm.clone()),
        ));
        let provider_type = llm
            .provider_config
            .as_ref()
            .map(|p| p.to_agent_core_provider_type())
            .unwrap_or_else(|| agent_core::ProviderType::ApiBased {
                protocol: agent_core::ApiProtocol::OpenAiLegacy,
                api_key: String::new(),
                reasoning_key: None,
            });
        let base_url = llm
            .provider_config
            .as_ref()
            .map(|p| p.base_url.clone())
            .unwrap_or_default();
        let brain_config = agent_core::BrainConfig {
            system_prompt: self.agent.system_prompt.clone(),
            base_url,
            model: llm.model_name.clone(),
            provider_type,
            max_steps_per_turn: self.max_steps_per_turn,
            max_step_attempts: self.max_retries_per_step,
            provider: None,
            provider_factory: Arc::new(agent_core::DefaultProviderFactory),
            approval_runtime: Arc::new(agent_core::core::approval::DefaultApprovalRuntime::new(
                Arc::new(agent_core::core::approval::AutoApprove),
            )),
            tool_sources: Vec::new(),
            toolset: Some(Arc::new(crate::soul::toolset::KimiToolsetHandle(
                self.toolset.clone(),
            ))),
            message_store,
            compaction_policy: Some(compaction_policy),
            injection_policy: Some(self.injection_policy.clone()),
            hook_policy,
            retry_policy,
            recovery_policy,
            step_policy: Some(self.step_policy.clone()),
            checkpoint_policy: Some(self.checkpoint_policy.clone()),
            system_prompt_policy: Arc::new(
                agent_core::core::system_prompt::DefaultSystemPromptPolicy,
            ),
            tool_result_transformer: None,
            event_policy: None,
        };
        self.brain = Some(
            agent_core::Brain::new(brain_config)
                .map_err(|e| OctopusError::Other(format!("Brain initialization failed: {e}")))?,
        );
        Ok(())
    }

    async fn maybe_load_mcp_tools(&mut self) {
        // Only the first turn can get mutable access to the toolset, before Brain
        // clones the Arc. On later turns the deferred load has already been consumed.
        if self.brain.is_some() {
            return;
        }

        let mcp_started = std::sync::Arc::get_mut(&mut self.toolset)
            .unwrap()
            .start_deferred_mcp_tool_loading()
            .await;
        let mut mcp_was_loading = false;
        if mcp_started && let Some(snapshot) = self.toolset.mcp_status_snapshot() {
            mcp_was_loading = snapshot.loading;
            if mcp_was_loading {
                wire_send(crate::wire::WireEvent::StatusUpdate(
                    crate::wire::StatusUpdate {
                        mcp_status: Some(snapshot),
                        ..Default::default()
                    },
                ));
                wire_send(crate::wire::WireEvent::McpLoadingBegin(
                    crate::wire::MCPLoadingBegin {},
                ));
            }
        }
        if mcp_was_loading {
            std::sync::Arc::get_mut(&mut self.toolset)
                .unwrap()
                .wait_for_mcp_tools()
                .await;
            if let Some(mcp_snap) = self.toolset.mcp_status_snapshot() {
                if mcp_snap.connected > 0 {
                    crate::track!(
                        "mcp_connected",
                        server_count = mcp_snap.connected,
                        total_count = mcp_snap.total,
                    );
                }
                let failed = mcp_snap.total.saturating_sub(mcp_snap.connected);
                if failed > 0 {
                    crate::track!(
                        "mcp_failed",
                        failed_count = failed,
                        total_count = mcp_snap.total,
                    );
                }
                wire_send(crate::wire::WireEvent::StatusUpdate(
                    crate::wire::StatusUpdate {
                        mcp_status: Some(mcp_snap),
                        ..Default::default()
                    },
                ));
                wire_send(crate::wire::WireEvent::McpLoadingEnd(
                    crate::wire::MCPLoadingEnd {},
                ));
            }
        }
    }

    async fn run_turn_body(&mut self, text: &str) -> Result<String> {
        let user_message = Message {
            role: "user".to_string(),
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            tool_call_id: None,
            tool_calls: None,
        };

        // Slash command handling
        if let Some(command_call) = parse_slash_command_call(text) {
            let cmd_name = command_call.name.clone();
            let cmd_args = command_call.args.clone();
            if let Some(command) = self.slash_registry.get(&cmd_name) {
                let func = command.func.clone();
                drop(command);
                (func)(self, &cmd_args).await;
                return Ok(String::new());
            }
        }

        // --- UserPromptSubmit hook ---
        let event = HookEvent::user_prompt_submit(
            &self.session.id,
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            text,
        );
        if self.hook_engine.has_hooks_for(event.kind()) {
            let results = self.hook_engine.trigger(event).await;
            for r in &results {
                if let crate::hooks::runner::HookAction::Block(ref reason) = r.action {
                    wire_send(crate::wire::WireEvent::TextPart(crate::wire::TextPart {
                        text: format!("UserPromptSubmit hook blocked: {}", reason),
                    }));
                    return Ok(String::new());
                }
            }
        }

        wire_send(crate::wire::WireEvent::TurnBegin(TurnBegin {
            user_input: Some(text.to_string()),
        }));

        self.maybe_load_mcp_tools().await;
        self.ensure_brain().await?;
        let turn_result = self.turn(user_message).await;

        // TurnEnd is sent regardless of success or failure.
        wire_send(crate::wire::WireEvent::TurnEnd(TurnEnd {}));
        if turn_result.is_err() {
            tracing::warn!("Turn interrupted at step {}", self.current_step_no);
            crate::track!(
                "turn_interrupted",
                step_no = self.current_step_no,
                session_id = self.session.id,
            );
        } else {
            // --- Stop hook (normal turn completion) ---
            let event = HookEvent::stop(
                &self.session.id,
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string()),
                false,
            );
            drop(self.hook_engine.fire_and_forget_trigger(event));
        }

        match turn_result {
            Ok(outcome) => {
                if let Some(msg) = outcome.final_message {
                    Ok(msg.extract_text(" "))
                } else {
                    Ok(String::new())
                }
            }
            Err(e) => Err(e),
        }
    }

    fn start_notification_pump(
        &self,
        soul_side: crate::wire::channel::WireSoulSide,
    ) -> Option<tokio::task::JoinHandle<()>> {
        // Only root pumps notifications to wire.
        let nm = self.notification_manager.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let _ = nm
                    .deliver_pending("wire", 8, |view| {
                        let notification = crate::notifications::wire::to_wire_notification(view);
                        soul_side.send(crate::wire::WireEvent::Notification(notification));
                        std::future::ready(())
                    })
                    .await;
            }
        }))
    }

    async fn turn(&mut self, user_message: Message) -> Result<TurnOutcome> {
        if self.llm.is_none() {
            return Err(LLMNotSet::NotSet.into());
        }

        if let Some(ref llm) = self.llm {
            let missing = check_message(&user_message, &llm.capabilities);
            if !missing.is_empty() {
                return Err(LLMNotSupported::NotSupported(format!("{:?}", missing)).into());
            }
        }

        self.current_turn_id = Some(uuid::Uuid::new_v4().to_string());
        self.step_policy
            .set_current_turn_id(self.current_turn_id.clone());

        {
            let mut ctx = self.context.lock().await;
            ctx.append_message(user_message)
                .await
                .map_err(OctopusError::Io)?;
        }

        self.agent_loop().await
    }

    async fn agent_loop(&mut self) -> Result<TurnOutcome> {
        // Discard stale steers and consume any new ones before the turn starts.
        self.steer_queue.clear();
        let _ = self.consume_pending_steers().await;

        let mut step_no = 0;
        loop {
            step_no += 1;
            if step_no > self.max_steps_per_turn {
                return Err(MaxStepsReached::Reached.into());
            }

            self.current_step_no = step_no;
            self.step_policy.set_current_step_no(step_no);
            wire_send(crate::wire::WireEvent::StepBegin(StepBegin { n: step_no }));

            let step_result = match self.run_brain_step().await {
                Ok(result) => result,
                Err(e) => {
                    wire_send(crate::wire::WireEvent::StepInterrupted(StepInterrupted {}));
                    // --- StopFailure hook ---
                    // Fire-and-forget: hook execution must not block error propagation.
                    let event = HookEvent::stop_failure(
                        &self.session.id,
                        std::env::current_dir()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| ".".to_string()),
                        std::any::type_name_of_val(&e),
                        format!("{}", e),
                    );
                    drop(self.hook_engine.fire_and_forget_trigger(event));
                    return Err(e);
                }
            };

            // Consume pending steers
            let has_steers = self.consume_pending_steers().await;
            if has_steers {
                continue;
            }

            if let Some(outcome) = step_result {
                let stop_reason = outcome.stop_reason.clone();
                return Ok(TurnOutcome {
                    final_message: if stop_reason == "no_tool_calls" {
                        Some(outcome.assistant_message)
                    } else {
                        None
                    },
                });
            }
        }
    }

    /// The actual LLM step logic, using Brain::run_step.
    async fn run_brain_step(&mut self) -> Result<Option<StepOutcome>> {
        let brain = self
            .brain
            .as_mut()
            .ok_or_else(|| OctopusError::Other("Brain not initialized".to_string()))?;
        let mut stream = brain
            .run_step()
            .await
            .map_err(|e| OctopusError::Other(format!("Brain step failed: {e}")))?;

        let mut assistant_content: Vec<ContentPart> = Vec::new();
        let mut tool_results: Vec<ToolResult> = Vec::new();
        let mut any_tool_error = false;

        while let Some(event) = stream.next().await {
            match event {
                agent_core::BrainEvent::TextPart(text) => {
                    assistant_content.push(ContentPart::Text { text: text.clone() });
                    wire_send(crate::wire::WireEvent::TextPart(TextPart { text }));
                }
                agent_core::BrainEvent::ThinkingPart(think) => {
                    assistant_content.push(ContentPart::Think {
                        think: think.clone(),
                    });
                    wire_send(crate::wire::WireEvent::ContentPart(ContentPart::Think {
                        think,
                    }));
                }
                agent_core::BrainEvent::ToolResult {
                    id,
                    output,
                    is_error,
                } => {
                    any_tool_error |= is_error;
                    let wire_result = ToolResult {
                        tool_call_id: id,
                        return_value: crate::wire::ToolReturnValue {
                            is_error,
                            output: Some(crate::wire::ToolOutput::Text(output.clone())),
                            message: Some(output),
                            brief: None,
                        },
                    };
                    wire_send(crate::wire::WireEvent::ToolResult(wire_result.clone()));
                    tool_results.push(wire_result);
                }
                agent_core::BrainEvent::Usage { usage } => {
                    let wire_usage = llm_to_wire_usage(usage);
                    {
                        let mut ctx = self.context.lock().await;
                        ctx.update_token_count(wire_usage.input)
                            .await
                            .map_err(OctopusError::Io)?;
                    }
                    let status_update = StatusUpdate {
                        token_usage: Some(wire_usage),
                        message_id: None,
                        plan_mode: Some(self.plan_mode),
                        context_usage: Some(self.context_usage().await),
                        context_tokens: Some(self.context.lock().await.token_count()),
                        max_context_tokens: self.llm.as_ref().map(|l| l.max_context_size),
                        ..Default::default()
                    };
                    wire_send(crate::wire::WireEvent::StatusUpdate(status_update));
                }
                agent_core::BrainEvent::Error(e) => {
                    return Err(crate::llm::classify_kosong_error(e));
                }
                _ => {}
            }
        }

        let build_assistant_message = |content: Vec<ContentPart>| Message {
            role: "assistant".to_string(),
            content,
            tool_call_id: None,
            tool_calls: None,
        };

        if !tool_results.is_empty() {
            if any_tool_error {
                return Ok(Some(StepOutcome {
                    stop_reason: "tool_rejected".to_string(),
                    assistant_message: build_assistant_message(assistant_content),
                }));
            }

            Ok(None)
        } else {
            Ok(Some(StepOutcome {
                stop_reason: "no_tool_calls".to_string(),
                assistant_message: build_assistant_message(assistant_content),
            }))
        }
    }

    async fn context_usage(&self) -> f64 {
        let max_size = self.llm.as_ref().map(|l| l.max_context_size).unwrap_or(0);
        if max_size > 0 {
            self.context.lock().await.token_count() as f64 / max_size as f64
        } else {
            0.0
        }
    }

    async fn consume_pending_steers(&mut self) -> bool {
        let mut consumed = false;
        while let Some(content) = self.steer_queue.pop() {
            let steer_msg = Message {
                role: "user".to_string(),
                content: vec![ContentPart::Text {
                    text: content.clone(),
                }],
                tool_call_id: None,
                tool_calls: None,
            };
            if let Err(e) = self.context.lock().await.append_message(steer_msg).await {
                tracing::warn!("Failed to append steer message: {}", e);
            }
            wire_send(crate::wire::WireEvent::SteerInput(SteerInput {
                user_input: content,
            }));
            consumed = true;
        }
        consumed
    }

    pub fn steer(&mut self, content: &str) {
        self.steer_queue.push(content.to_string());
    }

    /// Sync the in-memory approval state to the session and persist it.
    pub(super) fn sync_approval_state(&mut self) {
        self.session.state.approval.mode = self.approval.state().mode;
        self.session.state.approval.auto_approve_actions = self.approval.auto_approve_actions();
        self.session.save_state();
    }

    pub async fn compact_context(&mut self, custom_instruction: &str) -> Result<()> {
        let llm = self.llm.as_ref().ok_or(LLMNotSet::NotSet)?;
        let (messages, token_count) = {
            let ctx = self.context.lock().await;
            (ctx.history().to_vec(), ctx.token_count())
        };
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // --- PreCompact hook ---
        let event = HookEvent::pre_compact(&self.session.id, &cwd, custom_instruction, token_count);
        if self.hook_engine.has_hooks_for(event.kind()) {
            let results = self.hook_engine.trigger(event).await;
            for r in &results {
                if let crate::hooks::runner::HookAction::Block(ref reason) = r.action {
                    tracing::warn!("PreCompact hook blocked compaction: {}", reason);
                    return Err(OctopusError::Other(format!(
                        "PreCompact hook blocked: {}",
                        reason
                    )));
                }
            }
        }

        wire_send(crate::wire::WireEvent::CompactionBegin(CompactionBegin {}));

        let compact_t0 = std::time::Instant::now();
        let compact_result = self
            .compaction
            .compact(&messages, llm, custom_instruction)
            .await;
        let compact_duration = compact_t0.elapsed();

        let result = match compact_result {
            Ok(r) => r,
            Err(e) => {
                crate::track!(
                    "compaction_failed",
                    trigger_type = custom_instruction,
                    before_tokens = token_count,
                    duration_ms = compact_duration.as_millis() as u64,
                    retry_count = 0,
                    error_type = std::any::type_name_of_val(&e),
                );
                return Err(e);
            }
        };

        let estimated = result.estimated_token_count();

        {
            let mut ctx = self.context.lock().await;
            ctx.clear().await.map_err(OctopusError::Io)?;
            ctx.write_system_prompt(&self.agent.system_prompt)
                .await
                .map_err(OctopusError::Io)?;
            ctx.checkpoint(false).await.map_err(OctopusError::Io)?;
            ctx.append_message(result.messages)
                .await
                .map_err(OctopusError::Io)?;
            ctx.update_token_count(estimated)
                .await
                .map_err(OctopusError::Io)?;
        }

        wire_send(crate::wire::WireEvent::CompactionEnd(CompactionEnd {}));

        crate::track!(
            "compaction_finished",
            trigger_type = custom_instruction,
            before_tokens = token_count,
            after_tokens = estimated,
            duration_ms = compact_duration.as_millis() as u64,
            retry_count = 0,
        );

        // --- PostCompact hook ---
        {
            let event =
                HookEvent::post_compact(&self.session.id, &cwd, custom_instruction, estimated);
            drop(self.hook_engine.fire_and_forget_trigger(event));
        }

        // Notify injection providers that history has been rebuilt so they can
        // reset any one-shot throttling state.
        self.notify_injection_providers_compacted().await;

        Ok(())
    }

    // ========================================================================
    // Dynamic Injection
    // ========================================================================

    async fn notify_injection_providers_compacted(&mut self) {
        for provider in &mut self.injection_providers {
            provider.on_context_compacted().await;
        }
    }

    pub async fn notify_afk_changed(&mut self, enabled: bool) {
        for provider in &mut self.injection_providers {
            provider.on_afk_changed(enabled).await;
        }
        self.sync_injection_policy();
    }

    fn sync_injection_policy(&mut self) {
        let pending_plan_activation = self.consume_pending_plan_activation_injection();
        let state = crate::soul::brain_bridge::InjectionState {
            plan_mode: self.plan_mode,
            effective_afk: self.approval.is_afk(),
            persisted_afk: self.approval.state().mode.is_afk(),
            plan_file_path: self.get_plan_file_path(),
            pending_plan_activation,
        };
        self.injection_policy.set_state(state);
    }

    // ========================================================================
    // Plan Mode
    // ========================================================================

    pub fn get_plan_file_path(&self) -> Option<PathBuf> {
        self.plan_session_id.as_ref().map(|id| {
            // TODO: use hero-name slug system like Python (heroes.py).
            // For now, use a deterministic path based on session id.
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".kimi")
                .join("plans")
                .join(format!("{id}.md"))
        })
    }

    pub fn consume_pending_plan_activation_injection(&mut self) -> bool {
        if !self.plan_mode || !self.pending_plan_activation_injection {
            return false;
        }
        self.pending_plan_activation_injection = false;
        true
    }

    pub fn set_plan_mode(&mut self, enabled: bool) {
        self.plan_mode = enabled;
        self.session.state.plan_mode = enabled;
        if enabled {
            if self.plan_session_id.is_none() {
                self.plan_session_id = Some(uuid::Uuid::new_v4().to_string());
                self.session.state.plan_session_id = self.plan_session_id.clone();
            }
            self.pending_plan_activation_injection = true;
        } else {
            self.pending_plan_activation_injection = false;
            self.plan_session_id = None;
            self.session.state.plan_session_id = None;
            self.session.state.plan_slug = None;
        }
        self.sync_injection_policy();
        self.session.save_state();
    }

    pub fn toggle_plan_mode(&mut self) -> bool {
        let new_state = !self.plan_mode;
        self.set_plan_mode(new_state);
        new_state
    }

    pub fn read_current_plan(&self) -> Option<String> {
        let path = self.get_plan_file_path()?;
        std::fs::read_to_string(&path).ok()
    }

    pub fn clear_current_plan(&self) {
        if let Some(path) = self.get_plan_file_path() {
            let _ = std::fs::remove_file(&path);
        }
    }

    pub async fn status_snapshot(&self) -> StatusSnapshot {
        let token_count = self.context.lock().await.token_count();
        let max_size = self.llm.as_ref().map(|l| l.max_context_size).unwrap_or(0);
        StatusSnapshot {
            context_usage: if max_size > 0 {
                token_count as f64 / max_size as f64
            } else {
                0.0
            },
            yolo_enabled: self.approval.is_yolo(),
            afk_enabled: self.approval.state().mode.is_afk(),
            plan_mode: self.plan_mode,
            context_tokens: token_count,
            max_context_tokens: max_size,
            mcp_status: None,
        }
    }

    pub fn list_slash_commands(&self) -> Vec<(String, String, Vec<String>)> {
        self.slash_registry
            .list_commands()
            .into_iter()
            .map(|cmd| {
                (
                    cmd.name.clone(),
                    cmd.description.clone(),
                    cmd.aliases.clone(),
                )
            })
            .collect()
    }
}

pub struct StepOutcome {
    pub stop_reason: String,
    pub assistant_message: Message,
}

pub struct TurnOutcome {
    pub final_message: Option<Message>,
}

pub struct StatusSnapshot {
    pub context_usage: f64,
    pub yolo_enabled: bool,
    pub afk_enabled: bool,
    pub plan_mode: bool,
    pub context_tokens: usize,
    pub max_context_tokens: usize,
    pub mcp_status: Option<crate::wire::MCPStatusSnapshot>,
}
