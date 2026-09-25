use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::approval_runtime::{ApprovalResponse, ApprovalScope};
use crate::exception::{OctopusError, Result};
use crate::hooks::runner::HookAction;
use crate::hooks::{HookEventKind, OnWireHook, WireHookHandle};
use crate::soul::KimiSoul;
use crate::wire::channel::Wire;
use crate::wire::file::WireFile;
use crate::wire::jsonrpc::*;
use crate::wire::{HookRequest, WireEvent};

// ============================================================================
// Pending request tracking
// ============================================================================

enum PendingRequest {
    Hook(WireHookHandle),
    Approval { request_id: String },
}

// ============================================================================
// Wire server
// ============================================================================

pub struct WireServer {
    soul: Arc<Mutex<KimiSoul>>,
    write_tx: Option<mpsc::UnboundedSender<Value>>,
    pending_requests: Arc<Mutex<HashMap<String, PendingRequest>>>,
    streaming: Arc<AtomicBool>,
    cancel_handle: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
}

impl WireServer {
    pub fn new(soul: KimiSoul) -> Self {
        Self {
            soul: Arc::new(Mutex::new(soul)),
            write_tx: None,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            streaming: Arc::new(AtomicBool::new(false)),
            cancel_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Run the wire server until stdin closes or an unrecoverable error occurs.
    pub async fn serve(mut self) -> Result<()> {
        let (write_tx, mut write_rx) = mpsc::unbounded_channel();
        self.write_tx = Some(write_tx);

        let server = Arc::new(self);

        // Read loop: parse JSON-RPC from stdin and dispatch.
        let read_server = server.clone();
        let read_handle = tokio::spawn(async move { WireServer::read_loop(read_server).await });

        // Write loop: serialize outbound messages to stdout.
        let write_handle = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            while let Some(msg) = write_rx.recv().await {
                let line = match serde_json::to_string(&msg) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Failed to serialize outbound message: {}", e);
                        continue;
                    }
                };
                if let Err(e) = stdout.write_all(line.as_bytes()).await {
                    tracing::error!("Failed to write to stdout: {}", e);
                    break;
                }
                if let Err(e) = stdout.write_all(b"\n").await {
                    tracing::error!("Failed to write newline: {}", e);
                    break;
                }
                if let Err(e) = stdout.flush().await {
                    tracing::error!("Failed to flush stdout: {}", e);
                    break;
                }
            }
        });

        // Root hub loop: subscribe to the session-level RootWireHub and forward
        // approval requests (and responses) to the client.
        let hub_server = server.clone();
        let hub_handle = tokio::spawn(async move { WireServer::root_hub_loop(hub_server).await });

        // Wait for the read loop to finish (stdin closed).
        let result = match read_handle.await {
            Ok(r) => r,
            Err(e) => Err(OctopusError::Other(format!("Read loop panicked: {}", e))),
        };

        // Cleanup: close the write channel and wait for loops to exit.
        drop(server);
        write_handle.await.ok();
        hub_handle.await.ok();

        result
    }

    // ========================================================================
    // I/O loops
    // ========================================================================

    async fn read_loop(server: Arc<WireServer>) -> Result<()> {
        let stdin = tokio::io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Parse the generic envelope first so we can route by method/id.
            let envelope: JSONRPCMessage = match serde_json::from_str(line) {
                Ok(m) => m,
                Err(e) => {
                    server.send_error_nullable(
                        None,
                        ErrorCodes::PARSE_ERROR,
                        format!("Invalid JSON: {}", e),
                    );
                    continue;
                }
            };

            if envelope.is_response() {
                // Try success first, then error.
                if let Ok(resp) = serde_json::from_str::<JSONRPCSuccessResponse>(line) {
                    let server = server.clone();
                    tokio::spawn(async move {
                        server
                            .handle_response(JSONRPCClientResponse::Success(resp))
                            .await;
                    });
                } else if let Ok(resp) = serde_json::from_str::<JSONRPCErrorResponse>(line) {
                    let server = server.clone();
                    tokio::spawn(async move {
                        server
                            .handle_response(JSONRPCClientResponse::Error(resp))
                            .await;
                    });
                } else {
                    server.send_error_nullable(
                        envelope.id,
                        ErrorCodes::INVALID_REQUEST,
                        "Invalid JSON-RPC response",
                    );
                }
                continue;
            }

            let method = match envelope.method {
                Some(m) => m,
                None => {
                    server.send_error_nullable(
                        envelope.id,
                        ErrorCodes::INVALID_REQUEST,
                        "Missing method",
                    );
                    continue;
                }
            };

            let inbound =
                match method.as_str() {
                    "initialize" => serde_json::from_str::<JSONRPCInitializeMessage>(line)
                        .map(JSONRPCInbound::Initialize),
                    "prompt" => serde_json::from_str::<JSONRPCPromptMessage>(line)
                        .map(JSONRPCInbound::Prompt),
                    "steer" => {
                        serde_json::from_str::<JSONRPCSteerMessage>(line).map(JSONRPCInbound::Steer)
                    }
                    "replay" => serde_json::from_str::<JSONRPCReplayMessage>(line)
                        .map(JSONRPCInbound::Replay),
                    "set_plan_mode" => serde_json::from_str::<JSONRPCSetPlanModeMessage>(line)
                        .map(JSONRPCInbound::SetPlanMode),
                    "cancel" => serde_json::from_str::<JSONRPCCancelMessage>(line)
                        .map(JSONRPCInbound::Cancel),
                    _ => {
                        if let Some(id) = envelope.id {
                            server.send_error(
                                id,
                                ErrorCodes::METHOD_NOT_FOUND,
                                format!("Unexpected method received: {}", method),
                            );
                        }
                        continue;
                    }
                };

            match inbound {
                Ok(msg) => {
                    // `initialize` is handled synchronously in the read loop so that
                    // subsequent messages are not dispatched until initialization is
                    // complete. All other methods are spawned as tasks so they can run
                    // concurrently with the read loop.
                    if matches!(msg, JSONRPCInbound::Initialize(_)) {
                        server.dispatch_msg(msg).await;
                    } else {
                        let server = server.clone();
                        tokio::spawn(async move { server.dispatch_msg(msg).await });
                    }
                }
                Err(e) => {
                    if let Some(id) = envelope.id {
                        server.send_error(
                            id,
                            ErrorCodes::INVALID_PARAMS,
                            format!("Invalid parameters for method `{}`: {}", method, e),
                        );
                    }
                }
            }
        }

        tracing::info!("stdin closed, Wire server exiting");
        Ok(())
    }

    async fn root_hub_loop(server: Arc<WireServer>) {
        let hub = {
            let soul = server.soul.lock().await;
            soul.root_wire_hub.clone()
        };

        let Some(hub) = hub else {
            return;
        };

        let mut rx = hub.subscribe();
        loop {
            match rx.recv().await {
                Ok(WireEvent::ApprovalRequest(req)) => {
                    server.pending_requests.lock().await.insert(
                        req.id.clone(),
                        PendingRequest::Approval {
                            request_id: req.id.clone(),
                        },
                    );
                    let envelope = JSONRPCRequestMessage::new(req.id.clone(), req);
                    server.send_json(&envelope);
                }
                Ok(event) => {
                    server.send_event(&event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }

    // ========================================================================
    // Dispatch
    // ========================================================================

    async fn dispatch_msg(&self, msg: JSONRPCInbound) {
        match msg {
            JSONRPCInbound::Initialize(m) => self.handle_initialize(m).await,
            JSONRPCInbound::Prompt(m) => self.handle_prompt(m).await,
            JSONRPCInbound::Steer(m) => self.handle_steer(m).await,
            JSONRPCInbound::Replay(m) => self.handle_replay(m).await,
            JSONRPCInbound::SetPlanMode(m) => self.handle_set_plan_mode(m).await,
            JSONRPCInbound::Cancel(m) => self.handle_cancel(m).await,
        }
    }

    // ========================================================================
    // Handlers
    // ========================================================================

    async fn handle_initialize(&self, msg: JSONRPCInitializeMessage) {
        if self.streaming.load(Ordering::SeqCst) {
            self.send_error(
                msg.id,
                ErrorCodes::INVALID_STATE,
                "An agent turn is already in progress",
            );
            return;
        }

        // Register external tools (accepted/rejected lists are tracked for the response).
        let mut accepted: Vec<String> = Vec::new();
        let mut rejected: Vec<Value> = Vec::new();
        {
            let mut soul = self.soul.lock().await;
            if let Some(tools) = msg.params.external_tools {
                if let Some(toolset) = std::sync::Arc::get_mut(&mut soul.toolset) {
                    for tool in tools {
                        let parameters = match tool.parameters {
                            Value::Object(obj) => obj,
                            other => {
                                let mut obj = serde_json::Map::new();
                                obj.insert("schema".to_string(), other);
                                obj
                            }
                        };
                        let ok = toolset.register_external_tool(
                            &tool.name,
                            &tool.description,
                            parameters,
                        );
                        match ok {
                            (true, _) => accepted.push(tool.name),
                            (false, Some(reason)) => {
                                rejected.push(serde_json::json!({
                                    "name": tool.name,
                                    "reason": reason,
                                }));
                            }
                            (false, None) => {
                                rejected.push(serde_json::json!({
                                    "name": tool.name,
                                    "reason": "invalid",
                                }));
                            }
                        }
                    }
                } else {
                    for tool in tools {
                        rejected.push(serde_json::json!({
                            "name": tool.name,
                            "reason": "toolset is shared with an active turn",
                        }));
                    }
                }
            }
        }

        // Register wire hook subscriptions from the client.
        if let Some(hooks) = msg.params.hooks {
            let mut subs: Vec<crate::hooks::WireHookSubscription> = Vec::new();
            for h in hooks {
                match parse_hook_event(&h.event) {
                    Some(event) => subs.push(crate::hooks::WireHookSubscription {
                        id: h.id,
                        event,
                        matcher: h.matcher,
                        compiled_matcher: None,
                        timeout: h.timeout,
                    }),
                    None => {
                        tracing::warn!("Ignoring unknown hook event from client: {}", h.event);
                    }
                }
            }
            if !subs.is_empty() {
                let mut soul = self.soul.lock().await;
                soul.hook_engine.add_wire_subscriptions(subs);
            }
        }

        let mut result = serde_json::json!({
            "protocol_version": WIRE_PROTOCOL_VERSION,
            "server": {
                "name": "octopus-cli",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "supports_question": true,
            },
        });

        if !accepted.is_empty() || !rejected.is_empty() {
            result["external_tools"] = serde_json::json!({
                "accepted": accepted,
                "rejected": rejected,
            });
        }

        // Collect slash commands from the soul.
        let slash_commands: Vec<Value> = {
            let soul = self.soul.lock().await;
            soul.slash_registry
                .list_commands()
                .into_iter()
                .map(|cmd| {
                    serde_json::json!({
                        "name": cmd.name,
                        "description": cmd.description,
                        "aliases": cmd.aliases,
                    })
                })
                .collect()
        };
        result["slash_commands"] = Value::Array(slash_commands);

        self.send_success(msg.id, result);
    }

    async fn handle_prompt(&self, msg: JSONRPCPromptMessage) {
        if self.streaming.swap(true, Ordering::SeqCst) {
            self.send_error(
                msg.id,
                ErrorCodes::INVALID_STATE,
                "An agent turn is already in progress",
            );
            return;
        }

        let text = msg.params.user_input;
        let wire_file_path = {
            let soul = self.soul.lock().await;
            soul.session.wire_file_path.clone()
        };
        let wire_file = WireFile::new(wire_file_path);
        let wire = Wire::new(Some(wire_file));

        // Forward events from the per-run Wire channel to the client.
        let mut ui_side = wire.ui_side();
        let write_tx = self.write_tx.clone();
        let forward_handle = tokio::spawn(async move {
            loop {
                match ui_side.recv().await {
                    Ok(event) => {
                        if let Some(ref tx) = write_tx {
                            let envelope = JSONRPCEventMessage::new(event);
                            if let Ok(v) = serde_json::to_value(&envelope) {
                                let _ = tx.send(v);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        // Build the client-side hook dispatch callback.
        let pending = self.pending_requests.clone();
        let write_tx = self.write_tx.clone();
        let on_wire_hook: OnWireHook = Arc::new(move |handle: WireHookHandle| {
            let pending = pending.clone();
            let write_tx = write_tx.clone();
            Box::pin(async move {
                let request = HookRequest {
                    id: handle.id.clone(),
                    subscription_id: handle.subscription_id.clone(),
                    event: handle.event_name.clone(),
                    target: handle.target.clone(),
                    input_data: handle.input_data.clone(),
                };
                let request_id = request.id.clone();
                pending
                    .lock()
                    .await
                    .insert(request_id.clone(), PendingRequest::Hook(handle));
                if let Some(ref tx) = write_tx {
                    let envelope = JSONRPCRequestMessage::new(request_id, request);
                    if let Ok(v) = serde_json::to_value(&envelope) {
                        let _ = tx.send(v);
                    }
                }
            })
        });

        // Run the soul turn in its own task so it can be aborted on cancel.
        let soul = self.soul.clone();
        let pending_cleanup = self.pending_requests.clone();
        let soul_handle = tokio::spawn(async move {
            let mut soul = soul.lock().await;
            let on_done = Arc::new(move |id: &str| {
                let pending = pending_cleanup.clone();
                let id = id.to_string();
                tokio::spawn(async move {
                    pending.lock().await.remove(&id);
                });
            });
            soul.hook_engine.set_on_wire_hook_done(Some(on_done));
            soul.run_with_wire(&text, &wire, Some(on_wire_hook)).await
        });

        let abort_handle = soul_handle.abort_handle();
        *self.cancel_handle.lock().await = Some(abort_handle);

        let result = soul_handle.await;

        *self.cancel_handle.lock().await = None;
        self.streaming.store(false, Ordering::SeqCst);
        forward_handle.await.ok();

        match result {
            Ok(Ok(_)) => {
                self.send_success(msg.id, serde_json::json!({"status": Statuses::FINISHED}));
            }
            Ok(Err(OctopusError::LLMNotSet(_))) => {
                self.send_error(msg.id, ErrorCodes::LLM_NOT_SET, "LLM is not set");
            }
            Ok(Err(OctopusError::LLMNotSupported(ref e))) => {
                self.send_error(msg.id, ErrorCodes::LLM_NOT_SUPPORTED, e.to_string());
            }
            Ok(Err(OctopusError::MaxStepsReached(_))) => {
                self.send_success(
                    msg.id,
                    serde_json::json!({
                        "status": Statuses::MAX_STEPS_REACHED,
                    }),
                );
            }
            Ok(Err(e)) => {
                self.send_error(msg.id, ErrorCodes::INTERNAL_ERROR, format!("{}", e));
            }
            Err(join_err) if join_err.is_cancelled() => {
                self.send_success(msg.id, serde_json::json!({"status": Statuses::CANCELLED}));
            }
            Err(join_err) => {
                tracing::error!("Soul task panicked: {}", join_err);
                self.send_error(
                    msg.id,
                    ErrorCodes::INTERNAL_ERROR,
                    format!("Soul task panicked: {}", join_err),
                );
            }
        }
    }

    async fn handle_steer(&self, msg: JSONRPCSteerMessage) {
        if !self.streaming.load(Ordering::SeqCst) {
            self.send_error(
                msg.id,
                ErrorCodes::INVALID_STATE,
                "No agent turn is in progress",
            );
            return;
        }

        let mut soul = self.soul.lock().await;
        soul.steer(&msg.params.user_input);
        self.send_success(msg.id, serde_json::json!({"status": Statuses::STEERED}));
    }

    async fn handle_replay(&self, msg: JSONRPCReplayMessage) {
        if self.streaming.load(Ordering::SeqCst) {
            self.send_error(
                msg.id,
                ErrorCodes::INVALID_STATE,
                "An agent turn is already in progress",
            );
            return;
        }

        // Replay is not yet implemented in Rust (the wire file has no iter_records).
        self.send_error(
            msg.id,
            ErrorCodes::INTERNAL_ERROR,
            "Replay not yet implemented",
        );
    }

    async fn handle_set_plan_mode(&self, msg: JSONRPCSetPlanModeMessage) {
        let mut soul = self.soul.lock().await;
        let new_state = msg.params.enabled;
        soul.plan_mode = new_state;
        self.send_success(
            msg.id,
            serde_json::json!({"status": "ok", "plan_mode": new_state}),
        );
    }

    async fn handle_cancel(&self, msg: JSONRPCCancelMessage) {
        if !self.streaming.load(Ordering::SeqCst) {
            self.send_error(
                msg.id,
                ErrorCodes::INVALID_STATE,
                "No agent turn is in progress",
            );
            return;
        }

        if let Some(handle) = self.cancel_handle.lock().await.take() {
            handle.abort();
        }

        self.send_success(msg.id, serde_json::Value::Object(Default::default()));
    }

    async fn handle_response(&self, resp: JSONRPCClientResponse) {
        let (id, result, error) = match resp {
            JSONRPCClientResponse::Success(s) => (s.id, Some(s.result), None),
            JSONRPCClientResponse::Error(e) => (e.id, None, Some(e.error)),
        };

        let pending = self.pending_requests.lock().await.remove(&id);

        match pending {
            Some(PendingRequest::Hook(handle)) => {
                if error.is_some() {
                    handle.resolve(HookAction::Allow);
                    return;
                }
                if let Some(result) = result {
                    if let Ok(body) = serde_json::from_value::<HookResponse>(result) {
                        let action = if body.action == "block" {
                            HookAction::Block(body.reason)
                        } else {
                            HookAction::Allow
                        };
                        handle.resolve(action);
                    } else {
                        tracing::warn!("Invalid hook response for id={}", id);
                        handle.resolve(HookAction::Allow);
                    }
                } else {
                    handle.resolve(HookAction::Allow);
                }
            }
            Some(PendingRequest::Approval { request_id }) => {
                let runtime = self.soul.lock().await.approval.runtime().clone();
                if let Some(err) = error {
                    runtime.resolve(
                        &request_id,
                        ApprovalResponse::Reject {
                            feedback: err.message,
                        },
                    );
                    return;
                }
                if let Some(result) = result {
                    if let Ok(body) = serde_json::from_value::<ApprovalResponseBody>(result) {
                        let response = match body.response.as_str() {
                            "approve" => ApprovalResponse::Allow {
                                scope: ApprovalScope::Once,
                            },
                            "approve_for_session" => ApprovalResponse::Allow {
                                scope: ApprovalScope::Session,
                            },
                            _ => ApprovalResponse::Reject {
                                feedback: body.feedback,
                            },
                        };
                        runtime.resolve(&request_id, response);
                    } else {
                        runtime.resolve(
                            &request_id,
                            ApprovalResponse::Reject {
                                feedback: "Invalid approval response".to_string(),
                            },
                        );
                    }
                }
            }
            None => {
                tracing::warn!("Received response for unknown request id={}", id);
            }
        }
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    fn send_success(&self, id: String, result: impl Serialize) {
        let resp = JSONRPCSuccessResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: serde_json::to_value(&result).unwrap_or_default(),
        };
        self.send_json(&resp);
    }

    fn send_error(&self, id: String, code: i32, message: impl Into<String>) {
        let resp = JSONRPCErrorResponse {
            jsonrpc: "2.0".to_string(),
            id,
            error: JSONRPCErrorObject::new(code, message),
        };
        self.send_json(&resp);
    }

    fn send_error_nullable(&self, id: Option<String>, code: i32, message: impl Into<String>) {
        let resp = JSONRPCErrorResponseNullableID {
            jsonrpc: "2.0".to_string(),
            id,
            error: JSONRPCErrorObject::new(code, message),
        };
        self.send_json(&resp);
    }

    fn send_event(&self, event: &impl Serialize) {
        let envelope = JSONRPCEventMessage::new(event);
        self.send_json(&envelope);
    }

    fn send_json(&self, value: &impl Serialize) {
        if let Some(ref tx) = self.write_tx
            && let Ok(v) = serde_json::to_value(value)
        {
            let _ = tx.send(v);
        }
    }
}

// ============================================================================
// Hook event parsing
// ============================================================================

fn parse_hook_event(name: &str) -> Option<HookEventKind> {
    serde_json::from_str(&format!("\"{}\"", name))
        .map_err(|e: serde_json::Error| {
            tracing::debug!("Failed to parse hook event '{}': {}", name, e);
            e
        })
        .ok()
}
