use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::wire::{ApprovalRequestEvent, ApprovalResponseEvent, RootWireHub, WireEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSourceKind {
    ForegroundTurn,
    BackgroundAgent,
}

#[derive(Debug, Clone)]
pub struct ApprovalSource {
    pub kind: ApprovalSourceKind,
    pub id: String,
    pub agent_id: Option<String>,
}

// ============================================================================
// Task-local approval source tracking (mirrors Python's ContextVar)
// ============================================================================

tokio::task_local! {
    static CURRENT_APPROVAL_SOURCE: ApprovalSource;
}

/// Get the current approval source for this async task, if one has been set.
pub fn get_current_approval_source_or_none() -> Option<ApprovalSource> {
    CURRENT_APPROVAL_SOURCE.try_with(|s| s.clone()).ok()
}

/// Run a future with the given approval source set as the current source.
/// The source is automatically cleared when the future completes.
pub async fn with_approval_source<T>(
    source: ApprovalSource,
    f: impl std::future::Future<Output = T>,
) -> T {
    CURRENT_APPROVAL_SOURCE.scope(source, f).await
}

#[derive(Debug, thiserror::Error)]
#[error("Approval request cancelled")]
pub struct ApprovalCancelledError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    Once,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApprovalResponse {
    Allow { scope: ApprovalScope },
    Reject { feedback: String },
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool_call_id: String,
    pub sender: String,
    pub action: String,
    pub description: String,
    pub feedback: String,
    pub display: Vec<crate::wire::DisplayBlock>,
    pub source: ApprovalSource,
    pub created_at: Instant,
    pub status: ApprovalStatus,
    pub resolved_at: Option<Instant>,
    pub response: Option<ApprovalResponse>,
}

/// Parameters for [`ApprovalRuntime::create_request`].
#[derive(Debug, Clone)]
pub struct CreateRequestParams {
    pub request_id: String,
    pub tool_call_id: String,
    pub sender: String,
    pub action: String,
    pub description: String,
    pub display: Vec<crate::wire::DisplayBlock>,
    pub source: ApprovalSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ApprovalRuntime {
    inner: Arc<Mutex<ApprovalRuntimeInner>>,
}

#[derive(Debug, Default)]
struct ApprovalRuntimeInner {
    requests: HashMap<String, ApprovalRequest>,
    waiters: HashMap<String, oneshot::Sender<ApprovalResponse>>,
    hub: Option<RootWireHub>,
}

impl ApprovalRuntime {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ApprovalRuntimeInner::default())),
        }
    }

    pub fn bind_root_wire_hub(&self, hub: &RootWireHub) {
        self.inner.lock().unwrap().hub = Some(hub.clone());
    }

    pub fn create_request(&self, params: CreateRequestParams) {
        let CreateRequestParams {
            request_id,
            tool_call_id,
            sender,
            action,
            description,
            display,
            source,
        } = params;
        let req = ApprovalRequest {
            id: request_id.clone(),
            tool_call_id,
            sender: sender.clone(),
            action: action.clone(),
            description,
            feedback: String::new(),
            display,
            source,
            created_at: Instant::now(),
            status: ApprovalStatus::Pending,
            resolved_at: None,
            response: None,
        };

        // Publish to wire hub
        {
            let inner = self.inner.lock().unwrap();
            if let Some(hub) = inner.hub.as_ref() {
                let event = ApprovalRequestEvent {
                    id: req.id.clone(),
                    tool_call_id: req.tool_call_id.clone(),
                    sender,
                    action,
                    description: req.description.clone(),
                    source_kind: req.source.kind,
                    source_id: req.source.id.clone(),
                    display: req.display.clone(),
                };
                hub.publish(WireEvent::ApprovalRequest(event));
            }
        }

        let mut inner = self.inner.lock().unwrap();
        inner.requests.insert(request_id, req);
    }

    pub async fn wait_for_response(
        &self,
        request_id: &str,
        timeout: Option<Duration>,
    ) -> Result<ApprovalResponse, ApprovalCancelledError> {
        let rx = {
            let mut inner = self.inner.lock().unwrap();

            // Check if already resolved or cancelled
            if let Some(req) = inner.requests.get(request_id) {
                match req.status {
                    ApprovalStatus::Resolved => {
                        return Ok(req.response.clone().unwrap_or(ApprovalResponse::Reject {
                            feedback: req.feedback.clone(),
                        }));
                    }
                    ApprovalStatus::Cancelled => {
                        return Err(ApprovalCancelledError);
                    }
                    ApprovalStatus::Pending => {}
                }
            }

            // Create a new waiter channel
            let (tx, rx) = oneshot::channel();
            inner.waiters.insert(request_id.to_string(), tx);
            rx
        };

        if let Some(duration) = timeout {
            match tokio::time::timeout(duration, rx).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => {
                    self.cancel_request(request_id, "approval channel closed");
                    Err(ApprovalCancelledError)
                }
                Err(_) => {
                    self.cancel_request(request_id, "approval timed out");
                    Err(ApprovalCancelledError)
                }
            }
        } else {
            match rx.await {
                Ok(response) => Ok(response),
                Err(_) => {
                    self.cancel_request(request_id, "approval channel closed");
                    Err(ApprovalCancelledError)
                }
            }
        }
    }

    pub fn resolve(&self, request_id: &str, response: ApprovalResponse) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let request = match inner.requests.get_mut(request_id) {
            Some(r) if r.status == ApprovalStatus::Pending => r,
            _ => return false,
        };

        request.status = ApprovalStatus::Resolved;
        request.response = Some(response.clone());
        request.resolved_at = Some(Instant::now());

        let feedback = match &response {
            ApprovalResponse::Reject { feedback } => feedback.clone(),
            _ => String::new(),
        };
        request.feedback = feedback.clone();

        // Clone needed data before releasing mutable borrow
        let hub = inner.hub.clone();
        let request_id_owned = request_id.to_string();

        // Notify waiters
        if let Some(tx) = inner.waiters.remove(request_id) {
            let _ = tx.send(response.clone());
        }
        drop(inner);

        // Publish response to wire hub
        if let Some(hub) = hub {
            let event = ApprovalResponseEvent {
                request_id: request_id_owned,
                response: match response {
                    ApprovalResponse::Allow { scope } => match scope {
                        ApprovalScope::Once => "approve".to_string(),
                        ApprovalScope::Session => "approve_for_session".to_string(),
                    },
                    ApprovalResponse::Reject { .. } => "reject".to_string(),
                },
                feedback,
            };
            hub.publish(WireEvent::ApprovalResponse(event));
        }

        true
    }

    pub fn cancel_by_source(&self, kind: ApprovalSourceKind, id: &str) {
        let request_ids: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner
                .requests
                .iter()
                .filter(|(_, req)| {
                    req.status == ApprovalStatus::Pending
                        && req.source.kind == kind
                        && req.source.id == id
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        for request_id in request_ids {
            self.cancel_request(&request_id, "turn ended");
        }
    }

    pub fn get_request(&self, id: &str) -> Option<ApprovalRequest> {
        let inner = self.inner.lock().unwrap();
        inner.requests.get(id).cloned()
    }

    pub fn list_pending(&self) -> Vec<ApprovalRequest> {
        let inner = self.inner.lock().unwrap();
        inner
            .requests
            .values()
            .filter(|r| r.status == ApprovalStatus::Pending)
            .cloned()
            .collect()
    }

    fn cancel_request(&self, request_id: &str, feedback: &str) {
        let mut inner = self.inner.lock().unwrap();
        let request = match inner.requests.get_mut(request_id) {
            Some(r) if r.status == ApprovalStatus::Pending => r,
            _ => return,
        };

        request.status = ApprovalStatus::Cancelled;
        request.feedback = feedback.to_string();
        request.resolved_at = Some(Instant::now());
        request.response = Some(ApprovalResponse::Reject {
            feedback: feedback.to_string(),
        });

        if let Some(tx) = inner.waiters.remove(request_id) {
            let _ = tx.send(ApprovalResponse::Reject {
                feedback: feedback.to_string(),
            });
        }
    }
}

impl Default for ApprovalRuntime {
    fn default() -> Self {
        Self::new()
    }
}
