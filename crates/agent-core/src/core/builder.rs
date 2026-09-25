use std::sync::Arc;

use llm_provider::ChatProvider;

use crate::core::approval::{ApprovalPolicy, ApprovalRuntime, DefaultApprovalRuntime};
use crate::core::checkpoint::CheckpointPolicy;
use crate::core::config::BrainConfig;
use crate::core::errors::BrainError;
use crate::core::events::EventPolicy;
use crate::core::provider::ProviderFactory;
use crate::core::recovery::RecoveryPolicy;
use crate::core::retry::RetryPolicy;
use crate::core::step::StepPolicy;
use crate::core::system_prompt::SystemPromptPolicy;
use crate::core::tool_result::ToolResultTransformer;
use crate::hooks::policy::HookPolicy;
use crate::session::compaction::CompactionPolicy;
use crate::session::injection::InjectionPolicy;
use crate::session::store::MessageStore;

/// Builds a [`Brain`](crate::core::Brain) with custom policies.
///
/// All policy slots have sensible defaults, so a frontend only needs to
/// override the pieces it cares about.
#[derive(Default)]
pub struct BrainBuilder {
    config: BrainConfig,
}

impl BrainBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(mut self, config: BrainConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_provider_factory(mut self, factory: Arc<dyn ProviderFactory>) -> Self {
        self.config.provider_factory = factory;
        self
    }

    pub fn with_provider(mut self, provider: Arc<dyn ChatProvider>) -> Self {
        self.config.provider = Some(provider);
        self
    }

    pub fn with_message_store(mut self, store: Arc<tokio::sync::Mutex<dyn MessageStore>>) -> Self {
        self.config.message_store = store;
        self
    }

    pub fn with_max_steps_per_turn(mut self, max: usize) -> Self {
        self.config.max_steps_per_turn = max;
        self
    }

    pub fn with_max_step_attempts(mut self, max: usize) -> Self {
        self.config.max_step_attempts = max;
        self
    }

    pub fn with_approval_runtime(mut self, runtime: Arc<dyn ApprovalRuntime>) -> Self {
        self.config.approval_runtime = runtime;
        self
    }

    pub fn with_approval_policy(mut self, policy: Arc<dyn ApprovalPolicy>) -> Self {
        self.config.approval_runtime = Arc::new(DefaultApprovalRuntime::new(policy));
        self
    }

    pub fn with_hook_policy(mut self, policy: Arc<dyn HookPolicy>) -> Self {
        self.config.hook_policy = policy;
        self
    }

    pub fn with_compaction_policy(mut self, policy: Arc<dyn CompactionPolicy>) -> Self {
        self.config.compaction_policy = Some(policy);
        self
    }

    pub fn with_injection_policy(mut self, policy: Arc<dyn InjectionPolicy>) -> Self {
        self.config.injection_policy = Some(policy);
        self
    }

    pub fn with_retry_policy(mut self, policy: Arc<dyn RetryPolicy>) -> Self {
        self.config.retry_policy = policy;
        self
    }

    pub fn with_recovery_policy(mut self, policy: Arc<dyn RecoveryPolicy>) -> Self {
        self.config.recovery_policy = policy;
        self
    }

    pub fn with_step_policy(mut self, policy: Arc<dyn StepPolicy>) -> Self {
        self.config.step_policy = Some(policy);
        self
    }

    pub fn with_checkpoint_policy(mut self, policy: Arc<dyn CheckpointPolicy>) -> Self {
        self.config.checkpoint_policy = Some(policy);
        self
    }

    pub fn with_system_prompt_policy(mut self, policy: Arc<dyn SystemPromptPolicy>) -> Self {
        self.config.system_prompt_policy = policy;
        self
    }

    pub fn with_tool_result_transformer(
        mut self,
        transformer: Arc<dyn ToolResultTransformer>,
    ) -> Self {
        self.config.tool_result_transformer = Some(transformer);
        self
    }

    pub fn with_event_policy(mut self, policy: Arc<dyn EventPolicy>) -> Self {
        self.config.event_policy = Some(policy);
        self
    }

    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.config.system_prompt = prompt;
        self
    }

    pub async fn build(mut self) -> Result<crate::core::Brain, BrainError> {
        if self.config.provider.is_none() {
            self.config.provider = Some(self.config.build_provider().await?);
        }
        crate::core::Brain::new(self.config)
    }
}
