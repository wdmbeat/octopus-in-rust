use crate::chat_provider::{ChatProvider, ChatProviderError, Part, StreamedMessagePart};
use crate::message::{ContentPart, Message, Role, TokenUsage, ToolCall};
use crate::tooling::Tool;
use futures::StreamExt;

/// Result of a generation step.
#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub id: Option<String>,
    pub message: Message,
    pub usage: Option<TokenUsage>,
}

/// Generate one message from the chat provider.
pub async fn generate(
    chat_provider: &dyn ChatProvider,
    system_prompt: &str,
    tools: &[Tool],
    history: &[Message],
    mut on_message_part: Option<&mut (dyn FnMut(StreamedMessagePart) + Send)>,
    mut on_tool_call: Option<&mut (dyn FnMut(ToolCall) + Send)>,
) -> Result<GenerateResult, ChatProviderError> {
    let mut message = Message {
        role: Role::Assistant,
        name: None,
        content: Vec::new(),
        tool_calls: None,
        tool_call_id: None,
        partial: None,
    };
    let mut pending_part: Option<Part> = None;

    let stream = chat_provider
        .generate(system_prompt, tools, history)
        .await?;
    let id = stream.id;
    let usage = stream.usage;

    let mut stream = stream.stream;
    while let Some(part) = stream.next().await {
        if let Some(ref mut cb) = on_message_part {
            cb(part.clone());
        }

        if let Some(ref mut pending) = pending_part {
            let merged = match (&mut *pending, &part) {
                (
                    Part::Content(ContentPart::Text { text: a }),
                    Part::Content(ContentPart::Text { text: b }),
                ) => {
                    a.push_str(b);
                    true
                }
                (
                    Part::Content(ContentPart::Think {
                        think: a,
                        encrypted: None,
                    }),
                    Part::Content(ContentPart::Think { think: b, .. }),
                ) => {
                    a.push_str(b);
                    true
                }
                (Part::ToolCall(tc), Part::ToolCallPart(tcp)) => tc.merge_in_place(tcp),
                _ => false,
            };
            if !merged {
                _message_append(&mut message, pending.clone());
                if let Part::ToolCall(tc) = pending
                    && let Some(ref mut cb) = on_tool_call
                {
                    cb(tc.clone());
                }
                pending_part = Some(part);
            }
        } else {
            pending_part = Some(part);
        }
    }

    if let Some(pending) = pending_part {
        _message_append(&mut message, pending.clone());
        if let Part::ToolCall(tc) = &pending
            && let Some(ref mut cb) = on_tool_call
        {
            cb(tc.clone());
        }
    }

    if message.content.is_empty()
        && message
            .tool_calls
            .as_ref()
            .map(|v| v.is_empty())
            .unwrap_or(true)
    {
        return Err(ChatProviderError::empty_response());
    }

    let has_think = message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Think { .. }));
    let has_text = message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Text { text } if !text.trim().is_empty()));
    if has_think
        && !has_text
        && message
            .tool_calls
            .as_ref()
            .map(|v| v.is_empty())
            .unwrap_or(true)
    {
        return Err(ChatProviderError::new(
            "The API returned a response containing only thinking content \
             without any text or tool calls. This usually indicates the \
             stream was interrupted or the output token budget was exhausted \
             during reasoning.",
        ));
    }

    Ok(GenerateResult { id, message, usage })
}

fn _message_append(message: &mut Message, part: StreamedMessagePart) {
    match part {
        Part::Content(cp) => message.content.push(cp),
        Part::ToolCall(tc) => {
            message.tool_calls.get_or_insert_with(Vec::new).push(tc);
        }
        Part::ToolCallPart(_) => {
            // orphaned part; ignore
        }
    }
}
