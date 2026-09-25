use serde::{Deserialize, Serialize};

use crate::hooks::event::HookEvent;

/// Structured JSON a hook may write to stdout when it exits 0.
#[derive(Debug, Deserialize)]
struct HookStdout {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Deserialize)]
struct HookSpecificOutput {
    #[serde(rename = "permissionDecision")]
    permission_decision: Option<String>,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: Option<String>,
}

/// Semantic decision produced by a hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "reason")]
pub enum HookAction {
    Allow,
    Block(String),
}

/// Result of a single hook execution (raw output + semantic decision).
#[derive(Debug, Clone)]
pub struct HookResult {
    pub action: HookAction,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

impl HookResult {
    pub fn allow() -> Self {
        Self {
            action: HookAction::Allow,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        }
    }

    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            action: HookAction::Block(reason.into()),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 2,
            timed_out: false,
        }
    }
}

/// Execute a single hook command. Fail-open: errors/timeouts -> allow.
pub async fn run_hook(
    command: &str,
    event: &HookEvent,
    timeout_secs: u64,
    cwd: Option<&std::path::Path>,
) -> HookResult {
    let json_input = match serde_json::to_vec(event) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Hook failed to serialize event: {}", e);
            return HookResult::allow();
        }
    };

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Hook failed to spawn: {}: {}", command, e);
            return HookResult::allow();
        }
    };

    let result = tokio::time::timeout(tokio::time::Duration::from_secs(timeout_secs), async {
        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                tracing::warn!("Hook failed to open stdin");
                let _ = child.start_kill();
                return None;
            }
        };

        if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut stdin, &json_input).await {
            tracing::warn!("Hook failed to write stdin: {}", e);
            return None;
        }
        drop(stdin); // Close stdin so the child sees EOF

        child.wait_with_output().await.ok()
    })
    .await;

    let output = match result {
        Ok(Some(o)) => o,
        Ok(None) => {
            return HookResult::allow();
        }
        Err(_) => {
            tracing::warn!("Hook timed out after {}s: {}", timeout_secs, command);
            return HookResult {
                action: HookAction::Allow,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: true,
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(0);

    // Exit 2 = block
    if exit_code == 2 {
        return HookResult {
            action: HookAction::Block(stderr.trim().to_string()),
            stdout,
            stderr,
            exit_code: 2,
            timed_out: false,
        };
    }

    // Exit 0 + JSON stdout = structured decision
    if exit_code == 0
        && !stdout.trim().is_empty()
        && let Ok(parsed) = serde_json::from_str::<HookStdout>(&stdout)
        && let Some(ref output) = parsed.hook_specific_output
        && output.permission_decision.as_deref() == Some("deny")
    {
        let reason = output
            .permission_decision_reason
            .clone()
            .unwrap_or_default();
        return HookResult {
            action: HookAction::Block(reason),
            stdout,
            stderr,
            exit_code: 0,
            timed_out: false,
        };
    }

    HookResult {
        action: HookAction::Allow,
        stdout,
        stderr,
        exit_code,
        timed_out: false,
    }
}
