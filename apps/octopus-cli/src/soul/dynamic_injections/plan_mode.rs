use crate::soul::dynamic_injection::{
    DynamicInjection, DynamicInjectionProvider, InjectionContext,
};
use crate::wire::Message;

/// Inject a reminder every N assistant turns.
const TURN_INTERVAL: usize = 5;
/// Every N-th reminder is the full version; others are sparse.
const FULL_EVERY_N: usize = 5;

/// Periodically injects read-only reminders while plan mode is active.
///
/// Throttling is inferred from history: scan backwards to the last
/// plan mode reminder and count assistant messages in between.
/// Only inject when the count exceeds `TURN_INTERVAL`.
pub struct PlanModeInjectionProvider {
    inject_count: usize,
}

impl Default for PlanModeInjectionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanModeInjectionProvider {
    pub fn new() -> Self {
        Self { inject_count: 0 }
    }
}

#[async_trait::async_trait]
impl DynamicInjectionProvider for PlanModeInjectionProvider {
    async fn get_injections(
        &mut self,
        history: &[Message],
        ctx: &InjectionContext<'_>,
    ) -> Vec<DynamicInjection> {
        if !ctx.plan_mode {
            self.inject_count = 0;
            return Vec::new();
        }

        let plan_path_str = ctx.plan_file_path.map(|p| p.to_string_lossy().to_string());
        let plan_exists = ctx.plan_file_path.map(|p| p.exists()).unwrap_or(false);

        // Manual toggles schedule a one-shot activation reminder for the next LLM step.
        if ctx.pending_plan_activation {
            self.inject_count = 1;
            if plan_exists {
                return vec![DynamicInjection {
                    typ: "plan_mode_reentry".to_string(),
                    content: reentry_reminder(plan_path_str.as_deref()),
                }];
            }
            return vec![DynamicInjection {
                typ: "plan_mode".to_string(),
                content: full_reminder(plan_path_str.as_deref(), plan_exists),
            }];
        }

        // Scan history backwards to find the last plan mode reminder.
        let mut turns_since_last = 0;
        let mut found_previous = false;
        for msg in history.iter().rev() {
            if msg.role == "user" && has_plan_reminder(msg) {
                found_previous = true;
                break;
            }
            if msg.role == "assistant" {
                turns_since_last += 1;
            }
        }

        // First time (no reminder in history yet) -> inject full version.
        if !found_previous {
            self.inject_count = 1;
            return vec![DynamicInjection {
                typ: "plan_mode".to_string(),
                content: full_reminder(plan_path_str.as_deref(), plan_exists),
            }];
        }

        // Not enough turns since last reminder -> skip.
        if turns_since_last < TURN_INTERVAL {
            return Vec::new();
        }

        // Inject.
        self.inject_count += 1;
        let is_full = self.inject_count % FULL_EVERY_N == 1;
        let content = if is_full {
            full_reminder(plan_path_str.as_deref(), plan_exists)
        } else {
            sparse_reminder(plan_path_str.as_deref())
        };
        vec![DynamicInjection {
            typ: "plan_mode".to_string(),
            content,
        }]
    }
}

fn has_plan_reminder(msg: &Message) -> bool {
    let sparse = sparse_reminder(None);
    let full = full_reminder(None, false);
    let keys = [
        sparse.split('.').next().unwrap_or(""),
        full.split('\n').next().unwrap_or(""),
    ];
    for part in &msg.content {
        if let crate::wire::ContentPart::Text { text } = part {
            for key in &keys {
                if !key.is_empty() && text.contains(key) {
                    return true;
                }
            }
        }
    }
    false
}

fn full_reminder(plan_file_path: Option<&str>, plan_exists: bool) -> String {
    let mut lines: Vec<String> = vec![
        "Plan mode is active. You MUST NOT make any edits \\
            (with the exception of the plan file below), run non-readonly tools, \\
            or otherwise make changes to the system. \\
            This supersedes any other instructions you have received."
            .to_string(),
    ];

    if let Some(path) = plan_file_path {
        lines.push(String::new());
        if plan_exists {
            lines.push(format!(
                "Plan file: {path} \\
                    (exists — read first, then update it with WriteFile or StrReplaceFile)"
            ));
        } else {
            lines.push(format!(
                "Plan file: {path} \\
                    (create it with WriteFile; once it exists, you can modify it with \\
                    WriteFile or StrReplaceFile)"
            ));
        }
        lines.push("This is the only file you are allowed to edit.".to_string());
    }

    lines.extend_from_slice(&[
        String::new(),
        "Workflow:".to_string(),
        "1. Understand — explore the codebase with Glob, Grep, ReadFile".to_string(),
        "2. Design — converge on the best approach; \\
            consider trade-offs but aim for a single recommendation"
            .to_string(),
        "3. Review — re-read key files to verify understanding".to_string(),
        "4. Write Plan — modify the plan file with WriteFile or StrReplaceFile. \\
            Use WriteFile if the plan file does not exist yet"
            .to_string(),
        "5. Exit — call ExitPlanMode for user approval".to_string(),
        String::new(),
        "## Handling multiple approaches".to_string(),
        "Keep it focused: at most 2-3 meaningfully different approaches. \\
            Do NOT pad with minor variations — if one approach is clearly \\
            superior, just propose that one."
            .to_string(),
        "When the best approach depends on user preferences, constraints, \\
            or context you don't have, use AskUserQuestion to clarify first. \\
            This helps you write a better, more targeted plan rather than \\
            dumping multiple options for the user to sort through."
            .to_string(),
        "When you do include multiple approaches in the plan, you MUST pass them \\
            as the `options` parameter when calling ExitPlanMode, so the user can \\
            select which approach to execute at approval time."
            .to_string(),
        "NEVER write multiple approaches in the plan and call ExitPlanMode without \\
            the `options` parameter — the user will only see Approve/Reject with \\
            no way to choose."
            .to_string(),
        String::new(),
        "AskUserQuestion is for clarifying missing requirements or user preferences \\
            that affect the plan."
            .to_string(),
        "Never ask about plan approval via text or AskUserQuestion.".to_string(),
        "Your turn must end with either AskUserQuestion \\
            (to clarify requirements or preferences) \\
            or ExitPlanMode (to request plan approval). \\
            Do NOT end your turn any other way."
            .to_string(),
        "Do NOT use AskUserQuestion to ask about plan approval or reference \\
            \"the plan\" — the user cannot see the plan until you call ExitPlanMode."
            .to_string(),
    ]);

    lines.join("\n")
}

fn sparse_reminder(plan_file_path: Option<&str>) -> String {
    let mut parts = vec!["Plan mode still active (see full instructions earlier).".to_string()];
    if let Some(path) = plan_file_path {
        parts.push(format!("Read-only except plan file ({path})."));
    } else {
        parts.push("Read-only.".to_string());
    }
    parts.extend_from_slice(&[
        "Use WriteFile or StrReplaceFile to modify the plan file. \\
            If it does not exist yet, create it with WriteFile first."
            .to_string(),
        "Use AskUserQuestion to clarify user preferences \\
            when it helps you write a better plan."
            .to_string(),
        "If the plan has multiple approaches, \\
            pass options to ExitPlanMode so the user can choose."
            .to_string(),
        "End turns with AskUserQuestion (for clarifications) or ExitPlanMode (for approval)."
            .to_string(),
        "Never ask about plan approval via text or AskUserQuestion.".to_string(),
    ]);
    parts.join(" ")
}

fn reentry_reminder(plan_file_path: Option<&str>) -> String {
    let mut lines: Vec<String> = vec![
        "Plan mode is active. You MUST NOT make any edits \\
            (with the exception of the plan file below), run non-readonly tools, \\
            or otherwise make changes to the system. \\
            This supersedes any other instructions you have received."
            .to_string(),
        String::new(),
        "## Re-entering Plan Mode".to_string(),
    ];

    if let Some(path) = plan_file_path {
        lines.push(format!(
            "A plan file exists at {path} from a previous planning session."
        ));
    } else {
        lines.push("A plan file from a previous planning session already exists.".to_string());
    }

    lines.extend_from_slice(&[
        "Before proceeding:".to_string(),
        "1. Read the existing plan file to understand what was previously planned".to_string(),
        "2. Evaluate the user's current request against that plan".to_string(),
        "3. If different task: replace the old plan with a fresh one. \\
            If same task: update the existing plan."
            .to_string(),
        "4. You may use WriteFile or StrReplaceFile to modify the plan file. \\
            If the file does not exist yet, create it with WriteFile first."
            .to_string(),
        "5. Use AskUserQuestion to clarify missing requirements \\
            or user preferences that affect the plan."
            .to_string(),
        "6. Always edit the plan file before calling ExitPlanMode.".to_string(),
        String::new(),
        "Your turn must end with either AskUserQuestion (to clarify requirements) \\
            or ExitPlanMode (to request plan approval)."
            .to_string(),
    ]);

    lines.join("\n")
}
