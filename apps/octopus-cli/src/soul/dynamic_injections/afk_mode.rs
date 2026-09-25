use crate::soul::dynamic_injection::{
    DynamicInjection, DynamicInjectionProvider, InjectionContext,
};
use crate::wire::Message;

const AFK_INJECTION_TYPE: &str = "afk_mode";

const AFK_PROMPT_ROOT: &str = "You are running in afk mode. No user is present to answer \
    questions or approve actions. All tool calls are auto-approved by \
    the harness.\n\
    - Do NOT call AskUserQuestion — it will be auto-dismissed with no \
    answer, wasting a turn. Make your best judgment and proceed.\n\
    - You CAN use EnterPlanMode / ExitPlanMode normally. They will be \
    auto-approved. Planning still helps you think before acting; use \
    it for non-trivial tasks, then exit and execute.\n\
    - Finish the user's request end-to-end in this run. Do not defer \
    decisions to a human.";

pub const AFK_DISABLED_REMINDER: &str = "Afk mode is now disabled. The user is back at the terminal and CAN answer \
    AskUserQuestion.\n\
    - Ignore any earlier afk mode reminders that said no user is present or \
    that you must not call AskUserQuestion.\n\
    - AskUserQuestion is available again when a decision genuinely changes \
    your next action. Do not ask routine confirmations or progress check-ins.\n\
    - Tool calls are no longer auto-approved by afk. They may still be \
    auto-approved if yolo mode remains active.";

/// Injects afk (away-from-keyboard) guidance when no user is present.
pub struct AfkModeInjectionProvider {
    injected: bool,
}

impl Default for AfkModeInjectionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AfkModeInjectionProvider {
    pub fn new() -> Self {
        Self { injected: false }
    }
}

#[async_trait::async_trait]
impl DynamicInjectionProvider for AfkModeInjectionProvider {
    async fn get_injections(
        &mut self,
        _history: &[Message],
        ctx: &InjectionContext<'_>,
    ) -> Vec<DynamicInjection> {
        if !ctx.effective_afk {
            return Vec::new();
        }
        if !ctx.persisted_afk {
            return Vec::new();
        }

        if self.injected {
            return Vec::new();
        }
        self.injected = true;
        vec![DynamicInjection {
            typ: AFK_INJECTION_TYPE.to_string(),
            content: AFK_PROMPT_ROOT.to_string(),
        }]
    }

    async fn on_context_compacted(&mut self) {
        // Compaction rewrites history; the prior afk reminder may have been
        // summarized away, so let the next afk step restate the constraint.
        self.injected = false;
    }

    async fn on_afk_changed(&mut self, _enabled: bool) {
        // A runtime toggle changes the latest truth about user presence.
        // Re-arm so the next LLM step can inject the current afk guidance.
        self.injected = false;
    }
}
