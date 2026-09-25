use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::hooks::HookEvent;
use crate::mcp::{McpConfig, McpServerInfo, McpServerStatus};
use crate::wire::ToolCall as WireToolCall;

tokio::task_local! {
    static CURRENT_TOOL_CALL: Option<WireToolCall>;
}

pub fn get_current_tool_call() -> Option<WireToolCall> {
    CURRENT_TOOL_CALL.try_with(|tc| tc.clone()).unwrap_or(None)
}

const DEDUP_REMINDER_TEXT: &str = "\n\n<system-reminder>\n\
    You are repeating the exact same tool call with identical parameters.\
    Please carefully analyze the previous result. If the task is not yet complete,\
    try a different method or parameters instead of repeating the same call.\
    \n</system-reminder>";

/// Append dedup reminder text to an llm-provider [`ToolReturnValue`] output.
fn append_dedup_reminder(
    mut rv: llm_provider::tooling::ToolReturnValue,
) -> llm_provider::tooling::ToolReturnValue {
    let reminder = DEDUP_REMINDER_TEXT.to_string();

    match &mut rv.output {
        None => {
            rv.output = Some(Value::String(reminder));
        }
        Some(Value::String(text)) => {
            text.push_str(&reminder);
        }
        Some(_other) => {
            // For array/object outputs, we can't easily append — just set message
            if let Some(msg) = &mut rv.message {
                msg.push_str(&reminder);
            } else {
                rv.message = Some(reminder);
            }
        }
    }

    rv
}

/// Mutable state scoped to a single step, protected by a mutex so that
/// [`KimiToolset::handle`] can take `&self` and be called concurrently.
struct StepState {
    previous_step_calls: Vec<(String, String)>,
    current_step_calls: Vec<(String, String)>,
    current_step_results: HashMap<(String, String), llm_provider::tooling::ToolResult>,
    dedup_triggered: bool,
    step_no: usize,
    turn_id: String,
}

/// MCP mutable state, protected by a mutex.
struct McpState {
    deferred_mcp_load: Option<(Vec<McpConfig>, McpLoadContext)>,
    mcp_loading_task: Option<tokio::task::JoinHandle<()>>,
}

pub struct KimiToolset {
    tools: HashMap<String, Box<dyn llm_provider::tooling::CallableTool>>,
    /// Tool names that are registered but hidden from the LLM tool list.
    ///
    /// Hidden tools are still callable (e.g. by name via `find`), but they are
    /// excluded from `tools()` which returns the visible set sent to the LLM.
    /// This is used primarily for subagent `ToolPolicy::AllowList` — instead of
    /// rebuilding the toolset per subagent, we register all tools once and hide
    /// the ones the agent spec disallows.
    hidden_tools: HashSet<String>,
    hook_engine: crate::hooks::HookEngine,
    session_id: String,
    cwd: String,
    step_state: std::sync::Mutex<StepState>,
    // MCP state
    mcp_servers: HashMap<String, McpServerInfo>,
    mcp_state: std::sync::Mutex<McpState>,
    // Approval
    approval: std::sync::Mutex<Option<crate::soul::approval::Approval>>,
}

/// Context needed for deferred MCP loading.
#[derive(Clone)]
pub struct McpLoadContext {
    pub tool_call_timeout_ms: u64,
}

impl KimiToolset {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            hidden_tools: HashSet::new(),
            hook_engine: crate::hooks::HookEngine::default(),
            session_id: String::new(),
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            step_state: std::sync::Mutex::new(StepState {
                previous_step_calls: Vec::new(),
                current_step_calls: Vec::new(),
                current_step_results: HashMap::new(),
                dedup_triggered: false,
                step_no: 0,
                turn_id: String::new(),
            }),
            mcp_servers: HashMap::new(),
            mcp_state: std::sync::Mutex::new(McpState {
                deferred_mcp_load: None,
                mcp_loading_task: None,
            }),
            approval: std::sync::Mutex::new(None),
        }
    }

    pub fn set_approval(&self, approval: Option<crate::soul::approval::Approval>) {
        *self.approval.lock().unwrap() = approval;
    }

    /// Tools that require user approval before execution.
    fn requires_approval(name: &str) -> bool {
        matches!(name, "Shell" | "WriteFile" | "StrReplaceFile" | "Agent")
    }

    pub fn set_hook_engine(&mut self, engine: crate::hooks::HookEngine) {
        self.hook_engine = engine;
    }

    pub fn set_session_id(&mut self, id: String) {
        self.session_id = id;
    }

    pub fn register(&mut self, tool: Box<dyn llm_provider::tooling::CallableTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Convenience: register a [`CallableTool2`] by wrapping it in a [`CallableTool2Adapter`].
    pub fn register_typed<T: llm_provider::tooling::CallableTool2 + 'static>(&mut self, tool: T) {
        self.register(Box::new(llm_provider::tooling::CallableTool2Adapter::new(
            tool,
        )));
    }

    pub fn find(&self, name: &str) -> Option<&dyn llm_provider::tooling::CallableTool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Hide a tool from the LLM tool list. Returns `true` if the tool exists.
    pub fn hide(&mut self, tool_name: &str) -> bool {
        if self.tools.contains_key(tool_name) {
            self.hidden_tools.insert(tool_name.to_string());
            true
        } else {
            false
        }
    }

    /// Hide all tools except those in the allowlist.
    pub fn hide_all_except(&mut self, allowed: &[crate::tools::tool_name::ToolName]) {
        let allowed: std::collections::HashSet<String> =
            allowed.iter().map(|t| t.name().to_string()).collect();
        let all_names: Vec<String> = self.tools.keys().cloned().collect();
        for name in all_names {
            if !allowed.contains(&name) {
                self.hide(&name);
            }
        }
    }

    /// Restore a hidden tool to the LLM tool list.
    pub fn unhide(&mut self, tool_name: &str) {
        self.hidden_tools.remove(tool_name);
    }

    /// Visible tools.
    pub fn tools(&self) -> Vec<&dyn llm_provider::tooling::CallableTool> {
        self.tools
            .values()
            .filter(|t| !self.hidden_tools.contains(t.name()))
            .map(|t| t.as_ref())
            .collect()
    }

    /// Called before each step to set up deduplication state.
    pub fn begin_step(
        &self,
        previous_calls: Vec<(String, String)>,
        step_no: usize,
        turn_id: String,
    ) {
        let mut state = self.step_state.lock().unwrap();
        state.previous_step_calls = previous_calls;
        state.current_step_calls = Vec::new();
        state.current_step_results = HashMap::new();
        state.dedup_triggered = false;
        state.step_no = step_no;
        state.turn_id = turn_id;
    }

    /// Called after each step to capture the calls made in this step.
    pub fn end_step(&self) -> Vec<(String, String)> {
        self.step_state.lock().unwrap().current_step_calls.clone()
    }

    /// Whether a cross-step duplicate was blocked in the current step.
    pub fn dedup_triggered(&self) -> bool {
        self.step_state.lock().unwrap().dedup_triggered
    }

    /// Core tool execution — works with llm-provider types directly.
    async fn handle_inner(
        &self,
        tool_call: &llm_provider::ToolCall,
    ) -> llm_provider::tooling::ToolResult {
        let args_str = tool_call.function.arguments.clone().unwrap_or_default();
        let call_key = (tool_call.function.name.clone(), args_str.clone());

        // --- Same-step dedup: wait for the original result and copy it ---
        {
            let state = self.step_state.lock().unwrap();
            if let Some(original) = state.current_step_results.get(&call_key) {
                tracing::warn!(
                    "Same-step dedup detected for tool '{}' at step {}",
                    call_key.0,
                    state.step_no
                );
                let args_hash = format!("{:x}", Sha256::digest(call_key.1.as_bytes()));
                crate::track!(
                    "tool_call_dedup_detected",
                    session_id = self.session_id,
                    turn_id = state.turn_id.clone(),
                    step_no = state.step_no,
                    tool_name = call_key.0.clone(),
                    dup_type = "same_step",
                    args_hash = &args_hash[..8.min(args_hash.len())],
                );
                return original.clone();
            }
        }

        let is_cross_step_dup = {
            let state = self.step_state.lock().unwrap();
            state.previous_step_calls.contains(&call_key)
        };

        if is_cross_step_dup {
            let step_no = self.step_state.lock().unwrap().step_no;
            tracing::warn!(
                "Cross-step dedup detected for tool '{}' at step {}",
                call_key.0,
                step_no
            );
            let args_hash = format!("{:x}", Sha256::digest(call_key.1.as_bytes()));
            {
                let mut state = self.step_state.lock().unwrap();
                state.dedup_triggered = true;
            }
            crate::track!(
                "tool_call_dedup_detected",
                session_id = self.session_id,
                turn_id = self.step_state.lock().unwrap().turn_id.clone(),
                step_no = step_no,
                tool_name = call_key.0.clone(),
                dup_type = "cross_step",
                args_hash = &args_hash[..8.min(args_hash.len())],
            );
        }

        let tool = match self.tools.get(&tool_call.function.name) {
            Some(t) => t.as_ref(),
            None => {
                let result = llm_provider::tooling::ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    return_value: llm_provider::tooling::ToolReturnValue::error(format!(
                        "Tool '{}' not found",
                        tool_call.function.name
                    )),
                };
                let mut state = self.step_state.lock().unwrap();
                state
                    .current_step_results
                    .insert(call_key.clone(), result.clone());
                state.current_step_calls.push(call_key);
                return result;
            }
        };

        let arguments: Value = match serde_json::from_str(&args_str) {
            Ok(v) => v,
            Err(e) => {
                let result = llm_provider::tooling::ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    return_value: llm_provider::tooling::ToolReturnValue::error(format!(
                        "JSON parse error: {e}"
                    )),
                };
                let mut state = self.step_state.lock().unwrap();
                state
                    .current_step_results
                    .insert(call_key.clone(), result.clone());
                state.current_step_calls.push(call_key);
                return result;
            }
        };

        // --- PreToolUse hook ---
        let tool_input_map = match arguments.as_object() {
            Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => std::collections::HashMap::new(),
        };

        let event = HookEvent::pre_tool_use(
            &self.session_id,
            &self.cwd,
            &tool_call.function.name,
            &tool_input_map,
            &tool_call.id,
        );
        if self.hook_engine.has_hooks_for(event.kind()) {
            let results = self.hook_engine.trigger(event).await;
            for r in &results {
                if let crate::hooks::runner::HookAction::Block(ref reason) = r.action {
                    let result = llm_provider::tooling::ToolResult {
                        tool_call_id: tool_call.id.clone(),
                        return_value: llm_provider::tooling::ToolReturnValue::error(reason.clone()),
                    };
                    let mut state = self.step_state.lock().unwrap();
                    state
                        .current_step_results
                        .insert(call_key.clone(), result.clone());
                    state.current_step_calls.push(call_key);
                    return result;
                }
            }
        }

        // --- Approval check ---
        let approval_opt = self.approval.lock().unwrap().clone();
        if let Some(ref approval) = approval_opt
            && Self::requires_approval(&tool_call.function.name)
        {
            let description = format!("{}({})", tool_call.function.name, args_str);
            let result = approval
                .request("Octopus", &tool_call.function.name, &description, None)
                .await;
            if let crate::soul::approval::ApprovalResult::Rejected { feedback } = result {
                let return_value = llm_provider::tooling::ToolReturnValue::error(feedback.clone());
                let result = llm_provider::tooling::ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    return_value,
                };
                let mut state = self.step_state.lock().unwrap();
                state
                    .current_step_results
                    .insert(call_key.clone(), result.clone());
                state.current_step_calls.push(call_key);
                return result;
            }
        }

        let tool_call_id = tool_call.id.clone();
        let t0 = std::time::Instant::now();

        let wire_tc = WireToolCall {
            id: tool_call.id.clone(),
            call_type: tool_call.call_type,
            function: crate::wire::ToolCallFunction {
                name: tool_call.function.name.clone(),
                arguments: args_str,
            },
        };
        let return_value = CURRENT_TOOL_CALL
            .scope(Some(wire_tc), async { tool.call_raw(arguments).await })
            .await;

        let elapsed = t0.elapsed();
        let duration_ms = elapsed.as_millis() as u64;

        if return_value.is_error {
            let msg = return_value.message.clone().unwrap_or_default();
            crate::track!(
                "tool_call",
                tool_name = tool_call.function.name,
                outcome = "error",
                duration_ms = duration_ms,
            );
            let _ = &msg; // used for hooks below
        } else {
            crate::track!(
                "tool_call",
                tool_name = tool_call.function.name,
                outcome = "success",
                duration_ms = duration_ms,
            );
        }

        let mut result = llm_provider::tooling::ToolResult {
            tool_call_id,
            return_value,
        };

        if is_cross_step_dup {
            result.return_value = append_dedup_reminder(result.return_value.clone());
        }

        // --- PostToolUse / PostToolUseFailure hooks ---
        if result.return_value.is_error {
            // PostToolUseFailure remains fire-and-forget
            let error_text = result
                .return_value
                .message
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string());
            let event = HookEvent::post_tool_use_failure(
                &self.session_id,
                &self.cwd,
                &tool_call.function.name,
                &tool_input_map,
                &error_text,
                &tool_call.id,
            );
            if self.hook_engine.has_hooks_for(event.kind()) {
                drop(self.hook_engine.fire_and_forget_trigger(event));
            }
        } else {
            // PostToolUse is awaited so hook stderr can be surfaced to the LLM
            let output_text = result
                .return_value
                .output
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let event = HookEvent::post_tool_use(
                &self.session_id,
                &self.cwd,
                &tool_call.function.name,
                &tool_input_map,
                &output_text[..output_text.len().min(2000)],
                &tool_call.id,
            );
            if self.hook_engine.has_hooks_for(event.kind()) {
                let hook_results = self.hook_engine.trigger(event).await;

                // Collect non-empty stderr from hooks for LLM visibility
                let mut hook_stderr_lines: Vec<String> = Vec::new();
                for hr in &hook_results {
                    let trimmed = hr.stderr.trim();
                    if !trimmed.is_empty() {
                        hook_stderr_lines.push(trimmed.to_string());
                    }
                }

                if !hook_stderr_lines.is_empty() {
                    let hook_output = hook_stderr_lines.join("\n");
                    match &mut result.return_value.message {
                        Some(msg) if !msg.is_empty() => {
                            msg.push_str("\n\n[post-tool-use-hooks]\n");
                            msg.push_str(&hook_output);
                        }
                        _ => {
                            result.return_value.message =
                                Some(format!("[post-tool-use-hooks]\n{hook_output}"));
                        }
                    }
                }
            }
        }

        {
            let mut state = self.step_state.lock().unwrap();
            state
                .current_step_results
                .insert(call_key.clone(), result.clone());
            state.current_step_calls.push(call_key);
        }

        result
    }

    // ========================================================================
    // External tools (Wire)
    // ========================================================================

    /// Register an external tool delivered via the wire protocol.
    /// Returns `(ok, reason)` where `reason` is None on success.
    pub fn register_external_tool(
        &mut self,
        name: &str,
        description: &str,
        parameters: serde_json::Map<String, serde_json::Value>,
    ) -> (bool, Option<String>) {
        if self.tools.contains_key(name) {
            return (
                false,
                Some("tool name conflicts with existing tool".to_string()),
            );
        }
        let tool = WireExternalTool {
            name: name.to_string(),
            description: description.to_string(),
            parameters: Value::Object(parameters),
        };
        self.register(Box::new(tool));
        (true, None)
    }

    // ========================================================================
    // MCP
    // ========================================================================

    pub fn mcp_servers(&self) -> &HashMap<String, McpServerInfo> {
        &self.mcp_servers
    }

    pub fn mcp_status_snapshot(&self) -> Option<crate::wire::MCPStatusSnapshot> {
        if self.mcp_servers.is_empty() {
            return None;
        }

        let servers: Vec<crate::wire::MCPServerSnapshot> = self
            .mcp_servers
            .iter()
            .map(|(name, info)| crate::wire::MCPServerSnapshot {
                name: name.clone(),
                status: info.status.as_str().to_string(),
                tools: info.tools.iter().map(|t| t.name.clone()).collect(),
            })
            .collect();

        Some(crate::wire::MCPStatusSnapshot {
            loading: self.has_pending_mcp_tools(),
            connected: self
                .mcp_servers
                .values()
                .filter(|s| s.status == McpServerStatus::Connected)
                .count(),
            total: self.mcp_servers.len(),
            tools: self.mcp_servers.values().map(|s| s.tools.len()).sum(),
            servers,
        })
    }

    pub fn defer_mcp_tool_loading(&mut self, configs: Vec<McpConfig>, context: McpLoadContext) {
        self.mcp_state.lock().unwrap().deferred_mcp_load = Some((configs, context));
    }

    pub fn has_deferred_mcp_tools(&self) -> bool {
        self.mcp_state.lock().unwrap().deferred_mcp_load.is_some()
    }

    pub async fn start_deferred_mcp_tool_loading(&mut self) -> bool {
        let configs = {
            let mut mcp = self.mcp_state.lock().unwrap();
            if mcp.deferred_mcp_load.is_none() {
                return false;
            }
            if mcp.mcp_loading_task.is_some() || !self.mcp_servers.is_empty() {
                mcp.deferred_mcp_load = None;
                return false;
            }
            let (configs, _context) = mcp.deferred_mcp_load.take().unwrap();
            configs
        };
        self.load_mcp_tools(configs, true).await;
        true
    }

    pub fn has_pending_mcp_tools(&self) -> bool {
        self.mcp_state
            .lock()
            .unwrap()
            .mcp_loading_task
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }

    pub async fn wait_for_mcp_tools(&mut self) {
        let task = {
            let mut mcp = self.mcp_state.lock().unwrap();
            mcp.mcp_loading_task.take()
        };
        if let Some(t) = task {
            let _ = t.await;
        }
    }

    async fn load_mcp_tools(&mut self, configs: Vec<McpConfig>, _in_background: bool) {
        // Set up pending server entries from config.
        for config in &configs {
            for server_name in config.servers.keys() {
                if !self.mcp_servers.contains_key(server_name) {
                    self.mcp_servers.insert(
                        server_name.clone(),
                        McpServerInfo::new(McpServerStatus::Pending),
                    );
                }
            }
        }

        tracing::info!("MCP tool loading started");
        for config in configs {
            for (server_name, server_config) in config.servers {
                tracing::info!(
                    "Connecting to MCP server '{}' (transport: {})",
                    server_name,
                    server_config.transport
                );

                if let Some(info) = self.mcp_servers.get_mut(&server_name) {
                    info.status = McpServerStatus::Connecting;
                }

                if server_config.transport != "stdio" {
                    tracing::warn!(
                        "MCP transport '{}' not yet supported; server '{}' marked as failed",
                        server_config.transport,
                        server_name
                    );
                    if let Some(info) = self.mcp_servers.get_mut(&server_name) {
                        info.status = McpServerStatus::Failed;
                    }
                    continue;
                }

                let command = server_config.command.unwrap_or_default();
                let args = server_config.args.unwrap_or_default();
                let env = server_config.env.unwrap_or_default();

                match crate::mcp::client::McpClient::connect_stdio(&command, &args, &env).await {
                    Ok(client) => match client.list_tools().await {
                        Ok(tools) => {
                            for tool in &tools {
                                let schema = serde_json::json!({
                                    "name": tool.name,
                                    "description": tool.description.clone().unwrap_or_default(),
                                    "parameters": tool.input_schema.clone(),
                                });
                                let mcp_tool = crate::mcp::McpTool::new(
                                    tool.name.clone(),
                                    tool.description.clone().unwrap_or_default(),
                                    schema.clone(),
                                    client.clone(),
                                );
                                self.register(Box::new(mcp_tool));

                                if let Some(info) = self.mcp_servers.get_mut(&server_name) {
                                    info.tools.push(crate::mcp::McpToolInfo {
                                        name: tool.name.clone(),
                                        description: tool.description.clone().unwrap_or_default(),
                                        schema,
                                    });
                                    info.client = Some(client.clone());
                                    info.status = McpServerStatus::Connected;
                                }
                            }
                            tracing::info!(
                                "MCP server '{}' connected with {} tools",
                                server_name,
                                tools.len()
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to list tools from MCP server '{}': {}",
                                server_name,
                                e
                            );
                            if let Some(info) = self.mcp_servers.get_mut(&server_name) {
                                info.status = McpServerStatus::Failed;
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!("Failed to connect to MCP server '{}': {}", server_name, e);
                        if let Some(info) = self.mcp_servers.get_mut(&server_name) {
                            info.status = McpServerStatus::Failed;
                        }
                    }
                }
            }
        }
        tracing::info!("MCP tool loading finished");
    }

    pub async fn cleanup_mcp(&mut self) {
        let task = {
            let mut mcp = self.mcp_state.lock().unwrap();
            mcp.deferred_mcp_load = None;
            mcp.mcp_loading_task.take()
        };
        if let Some(t) = task {
            t.abort();
            let _ = t.await;
        }

        for (_, info) in self.mcp_servers.iter_mut() {
            if let Some(client) = info.client.take() {
                let _ = client.shutdown().await;
            }
        }
        self.mcp_servers.clear();
    }
}

impl Default for KimiToolset {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// llm_provider::Toolset wrapper
// ============================================================================

/// Thin wrapper around `Arc<KimiToolset>` so that `handle` can clone the Arc
/// and spawn the tool execution into a `tokio::task`.
///
/// `Toolset::handle` takes `&self`, but `handle_inner` is async and must outlive
/// the borrow. The wrapper holds `Arc<KimiToolset>`, so `handle` can
/// `Arc::clone(&self.0)` and move the clone into the spawned task.
pub struct KimiToolsetHandle(pub std::sync::Arc<KimiToolset>);

impl llm_provider::Toolset for KimiToolsetHandle {
    fn tools(&self) -> Vec<llm_provider::Tool> {
        self.0
            .tools()
            .into_iter()
            .map(|t| llm_provider::Tool {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
                prompt_fragment: t.prompt_fragment().map(|s| s.to_string()),
            })
            .collect()
    }

    fn handle(&self, tool_call: &llm_provider::ToolCall) -> llm_provider::HandleResult {
        let inner = std::sync::Arc::clone(&self.0);
        let tc = tool_call.clone();
        let handle = tokio::spawn(async move { inner.handle_inner(&tc).await });
        llm_provider::HandleResult::Pending(handle)
    }
}

// ============================================================================
// WireExternalTool
// ============================================================================

/// A tool that delegates execution to an external client via the wire protocol.
pub struct WireExternalTool {
    name: String,
    description: String,
    parameters: Value,
}

#[async_trait::async_trait]
impl llm_provider::tooling::CallableTool for WireExternalTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        // WireExternalTool stores only the parameters schema.
        self.parameters.clone()
    }

    async fn call_raw(&self, _args: Value) -> llm_provider::tooling::ToolReturnValue {
        // The actual call is handled by the wire server — this should not be
        // invoked directly. If it is, return an error explaining the issue.
        llm_provider::tooling::ToolReturnValue::error(format!(
            "External tool '{}' must be called through the wire protocol, not directly.",
            self.name
        ))
    }
}
