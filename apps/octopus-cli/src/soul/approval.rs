use crate::approval_runtime::{
    ApprovalCancelledError, ApprovalResponse, ApprovalRuntime, ApprovalSource,
    get_current_approval_source_or_none,
};
use crate::exception::ToolRejectedError;
use serde::{Deserialize, Serialize};

/// Combined approval mode for the session.
///
/// Yolo and Afk are independent toggles in the underlying UI (a user can have
/// both on at the same time). The enum exhaustively models the four
/// combinations so that `match` sites are forced to handle every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Normal interactive mode — ask the user for every tool call.
    #[default]
    Ask,
    /// Explicit yolo mode — auto-approve everything.
    Yolo,
    /// AFK mode — no user present, auto-approve everything.
    Afk,
    /// Both yolo and AFK are active simultaneously.
    YoloAndAfk,
}

impl ApprovalMode {
    pub fn is_yolo(&self) -> bool {
        matches!(self, Self::Yolo | Self::YoloAndAfk)
    }

    pub fn is_afk(&self) -> bool {
        matches!(self, Self::Afk | Self::YoloAndAfk)
    }

    pub fn is_auto_approve(&self) -> bool {
        !matches!(self, Self::Ask)
    }

    /// Toggle the yolo component on or off, preserving the afk component.
    pub fn toggle_yolo(&mut self) {
        *self = match *self {
            Self::Ask => Self::Yolo,
            Self::Afk => Self::YoloAndAfk,
            Self::Yolo => Self::Ask,
            Self::YoloAndAfk => Self::Afk,
        };
    }

    /// Toggle the afk component on or off, preserving the yolo component.
    pub fn toggle_afk(&mut self) {
        *self = match *self {
            Self::Ask => Self::Afk,
            Self::Yolo => Self::YoloAndAfk,
            Self::Afk => Self::Ask,
            Self::YoloAndAfk => Self::Yolo,
        };
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalState {
    pub mode: ApprovalMode,
    pub auto_approve_actions: Vec<String>,
}

impl ApprovalState {
    pub fn is_auto_approve(&self) -> bool {
        self.mode.is_auto_approve()
    }
}

/// Result of an approval request.
#[derive(Debug, Clone)]
pub enum ApprovalResult {
    Approved,
    Rejected { feedback: String },
}

impl ApprovalResult {
    pub fn approved(&self) -> bool {
        matches!(self, Self::Approved)
    }

    pub fn feedback(&self) -> Option<&str> {
        match self {
            Self::Approved => None,
            Self::Rejected { feedback } => Some(feedback.as_str()),
        }
    }

    pub fn rejection_error(&self) -> ToolRejectedError {
        match self {
            Self::Approved => {
                panic!("rejection_error called on an Approved result")
            }
            Self::Rejected { feedback } => {
                if !feedback.is_empty() {
                    ToolRejectedError::with_feedback(
                        format!(
                            "The tool call is rejected by the user. User feedback: {}",
                            feedback
                        ),
                        format!("Rejected: {}", feedback),
                    )
                } else {
                    let is_subagent = get_current_approval_source_or_none()
                        .map(|s| s.agent_id.is_some())
                        .unwrap_or(false);
                    if is_subagent {
                        ToolRejectedError::new(
                            "The tool call is rejected by the user. Try a different approach to complete your task, or explain the limitation in your summary if no alternative is available. Do not retry the same tool call, and do not attempt to bypass this restriction through indirect means.",
                        )
                    } else {
                        ToolRejectedError::new("The tool call is rejected by the user.")
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct Approval {
    state: std::sync::Arc<std::sync::RwLock<ApprovalState>>,
    runtime: ApprovalRuntime,
    runtime_afk: std::sync::Arc<std::sync::RwLock<bool>>,
    on_change: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Approval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Approval")
            .field("state", &self.state)
            .field("runtime", &self.runtime)
            .field("runtime_afk", &self.runtime_afk)
            .finish_non_exhaustive()
    }
}

impl Approval {
    pub fn new(mode: ApprovalMode) -> Self {
        let state = ApprovalState {
            mode,
            ..Default::default()
        };
        Self {
            state: std::sync::Arc::new(std::sync::RwLock::new(state)),
            runtime: ApprovalRuntime::new(),
            runtime_afk: std::sync::Arc::new(std::sync::RwLock::new(false)),
            on_change: None,
        }
    }

    pub fn with_state(state: ApprovalState) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::RwLock::new(state)),
            runtime: ApprovalRuntime::new(),
            runtime_afk: std::sync::Arc::new(std::sync::RwLock::new(false)),
            on_change: None,
        }
    }

    pub fn share(&self) -> Self {
        Self {
            state: self.state.clone(),
            runtime: self.runtime.clone(),
            runtime_afk: self.runtime_afk.clone(),
            on_change: self.on_change.clone(),
        }
    }

    pub fn set_runtime(&mut self, runtime: ApprovalRuntime) {
        self.runtime = runtime;
    }

    pub fn runtime(&self) -> &ApprovalRuntime {
        &self.runtime
    }

    pub fn toggle_yolo(&self) {
        let mut state = self.state.write().unwrap();
        state.mode.toggle_yolo();
        drop(state);
        self.notify_change();
    }

    pub fn toggle_afk(&self) {
        let mut state = self.state.write().unwrap();
        let was_afk = state.mode.is_afk();
        state.mode.toggle_afk();
        if was_afk {
            let mut rt_afk = self.runtime_afk.write().unwrap();
            *rt_afk = false;
        }
        drop(state);
        self.notify_change();
    }

    pub fn set_runtime_afk(&self, afk: bool) {
        let mut rt_afk = self.runtime_afk.write().unwrap();
        *rt_afk = afk;
    }

    pub fn is_auto_approve(&self) -> bool {
        let state = self.state.read().unwrap();
        let rt_afk = self.runtime_afk.read().unwrap();
        state.mode.is_auto_approve() || *rt_afk
    }

    pub fn is_yolo(&self) -> bool {
        self.state.read().unwrap().mode.is_yolo()
    }

    pub fn is_afk(&self) -> bool {
        let state = self.state.read().unwrap();
        let rt_afk = self.runtime_afk.read().unwrap();
        state.mode.is_afk() || *rt_afk
    }

    pub fn is_runtime_afk(&self) -> bool {
        *self.runtime_afk.read().unwrap()
    }

    pub fn auto_approve_actions(&self) -> Vec<String> {
        self.state.read().unwrap().auto_approve_actions.clone()
    }

    pub fn state(&self) -> ApprovalState {
        self.state.read().unwrap().clone()
    }

    fn notify_change(&self) {
        if let Some(ref cb) = self.on_change {
            cb();
        }
    }

    pub async fn request(
        &self,
        sender: &str,
        action: &str,
        description: &str,
        display: Option<Vec<crate::wire::DisplayBlock>>,
    ) -> ApprovalResult {
        let tool_call = crate::soul::toolset::get_current_tool_call();

        if self.is_auto_approve() {
            return ApprovalResult::Approved;
        }

        {
            let state = self.state.read().unwrap();
            if state.auto_approve_actions.contains(&action.to_string()) {
                return ApprovalResult::Approved;
            }
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let display_blocks = display.unwrap_or_default();
        let source = get_current_approval_source_or_none().unwrap_or_else(|| ApprovalSource {
            kind: crate::approval_runtime::ApprovalSourceKind::ForegroundTurn,
            id: tool_call
                .as_ref()
                .map(|t| t.id.clone())
                .unwrap_or_else(|| request_id.clone()),
            agent_id: None,
        });

        self.runtime
            .create_request(crate::approval_runtime::CreateRequestParams {
                request_id: request_id.clone(),
                tool_call_id: tool_call.as_ref().map(|t| t.id.clone()).unwrap_or_default(),
                sender: sender.to_string(),
                action: action.to_string(),
                description: description.to_string(),
                display: display_blocks,
                source,
            });

        match self.runtime.wait_for_response(&request_id, None).await {
            Ok(response) => match response {
                ApprovalResponse::Allow { scope } => {
                    if matches!(scope, crate::approval_runtime::ApprovalScope::Session) {
                        let mut state = self.state.write().unwrap();
                        if !state.auto_approve_actions.contains(&action.to_string()) {
                            state.auto_approve_actions.push(action.to_string());
                        }
                        drop(state);
                        self.notify_change();
                    }
                    ApprovalResult::Approved
                }
                ApprovalResponse::Reject { feedback } => ApprovalResult::Rejected { feedback },
            },
            Err(ApprovalCancelledError) => {
                let record = self.runtime.get_request(&request_id);
                ApprovalResult::Rejected {
                    feedback: record.map(|r| r.feedback).unwrap_or_default(),
                }
            }
        }
    }
}
