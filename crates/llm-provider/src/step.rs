use crate::chat_provider::{ChatProvider, ChatProviderError, StreamedMessagePart};
use crate::generate::generate;
use crate::message::{Message, TokenUsage, ToolCall};
use crate::tooling::{HandleResult, ToolResult, Toolset};
use std::collections::HashMap;
use std::sync::Arc;

/// Result of a single agent step.
pub struct StepResult {
    pub id: Option<String>,
    pub message: Message,
    pub usage: Option<TokenUsage>,
    pub tool_calls: Vec<ToolCall>,
    tool_result_futures: HashMap<String, tokio::task::JoinHandle<ToolResult>>,
}

impl StepResult {
    pub async fn tool_results(self) -> Vec<ToolResult> {
        self.tool_results_with_callback(|_| {}).await
    }

    /// Await all tool results, calling `on_result` for each completed future.
    pub async fn tool_results_with_callback(
        self,
        mut on_result: impl FnMut(&ToolResult),
    ) -> Vec<ToolResult> {
        if self.tool_result_futures.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut futures = self.tool_result_futures;

        for tool_call in &self.tool_calls {
            if let Some(handle) = futures.remove(&tool_call.id) {
                match handle.await {
                    Ok(result) => {
                        on_result(&result);
                        results.push(result);
                    }
                    Err(e) => {
                        let result = ToolResult {
                            tool_call_id: tool_call.id.clone(),
                            return_value: crate::tooling::ToolReturnValue::error(format!(
                                "Tool execution failed: {e}"
                            )),
                        };
                        on_result(&result);
                        results.push(result);
                    }
                }
            }
        }

        // Cancel any remaining futures
        for (_, handle) in futures {
            handle.abort();
        }

        results
    }
}

/// Run one agent step: generate an LLM response and dispatch tool calls.
pub async fn step(
    chat_provider: &dyn ChatProvider,
    system_prompt: &str,
    toolset: &dyn Toolset,
    history: &[Message],
    on_message_part: Option<&mut (dyn FnMut(StreamedMessagePart) + Send)>,
) -> Result<StepResult, ChatProviderError> {
    step_with_callbacks(
        chat_provider,
        system_prompt,
        toolset,
        history,
        on_message_part,
        None::<ToolResultCallback>,
    )
    .await
}

/// Callback invoked when a tool result becomes available.
pub type ToolResultCallback = Arc<dyn Fn(&ToolResult) + Send + Sync>;

/// Run one agent step with optional callbacks for message parts and tool results.
///
/// `on_tool_result` fires eagerly when each individual tool future completes,
/// matching the original Python implementation's behavior.
pub async fn step_with_callbacks(
    chat_provider: &dyn ChatProvider,
    system_prompt: &str,
    toolset: &dyn Toolset,
    history: &[Message],
    on_message_part: Option<&mut (dyn FnMut(StreamedMessagePart) + Send)>,
    on_tool_result: Option<ToolResultCallback>,
) -> Result<StepResult, ChatProviderError> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut tool_result_futures: HashMap<String, tokio::task::JoinHandle<ToolResult>> =
        HashMap::new();

    let mut on_tool_call = |tool_call: ToolCall| {
        let id = tool_call.id.clone();
        let result = toolset.handle(&tool_call);
        let handle = match result {
            HandleResult::Ready(result) => tokio::spawn(async move { result }),
            HandleResult::Pending(handle) => handle,
        };

        // If an on_tool_result callback is provided, wrap the handle so the
        // callback fires as soon as the future completes (eagerly).
        let stored_handle = if let Some(ref cb) = on_tool_result {
            let cb = Arc::clone(cb);
            let tc_id = id.clone();
            tokio::spawn(async move {
                let result = match handle.await {
                    Ok(r) => r,
                    Err(e) => ToolResult {
                        tool_call_id: tc_id,
                        return_value: crate::tooling::ToolReturnValue::error(format!(
                            "Tool execution failed: {e}"
                        )),
                    },
                };
                cb(&result);
                result
            })
        } else {
            handle
        };

        tool_calls.push(tool_call);
        tool_result_futures.insert(id, stored_handle);
    };

    let result = generate(
        chat_provider,
        system_prompt,
        &toolset.tools(),
        history,
        on_message_part,
        Some(&mut on_tool_call),
    )
    .await;

    match result {
        Ok(gen_result) => Ok(StepResult {
            id: gen_result.id,
            message: gen_result.message,
            usage: gen_result.usage,
            tool_calls,
            tool_result_futures,
        }),
        Err(e) => {
            // Cancel all pending futures
            for (_, handle) in tool_result_futures {
                handle.abort();
            }
            Err(e)
        }
    }
}
