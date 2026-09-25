use std::collections::HashSet;

use crate::config::ModelCapability;
use crate::wire::{ContentPart, Message, ToolResult};

pub fn system(message: &str) -> ContentPart {
    ContentPart::Text {
        text: format!("<system>{message}</system>"),
    }
}

pub fn system_reminder(message: &str) -> ContentPart {
    ContentPart::Text {
        text: format!("<system-reminder>\n{message}\n</system-reminder>"),
    }
}

pub fn is_system_reminder_message(message: &Message) -> bool {
    if message.role != "user" || message.content.len() != 1 {
        return false;
    }
    match &message.content[0] {
        ContentPart::Text { text } => text.trim().starts_with("<system-reminder>"),
        _ => false,
    }
}

pub fn tool_result_to_message(tool_result: &ToolResult) -> Message {
    let rv = &tool_result.return_value;
    let content = if rv.is_error() {
        let mut message = rv.message.clone().unwrap_or_default();
        if message.is_empty() {
            message = "Tool execution failed".to_string();
        }
        message.push_str("\nThis is an unexpected error and the tool is probably not working.");
        let mut parts = vec![system(&format!("ERROR: {message}"))];
        if let Some(output) = &rv.output {
            parts.extend(output_to_content_parts(output));
        }
        parts
    } else {
        let mut parts: Vec<ContentPart> = Vec::new();
        if let Some(msg) = &rv.message {
            parts.push(system(msg));
        }
        if let Some(output) = &rv.output {
            parts.extend(output_to_content_parts(output));
        }
        if parts.is_empty() {
            parts.push(system("Tool output is empty."));
        } else if !parts.iter().any(|p| matches!(p, ContentPart::Text { .. })) {
            parts.insert(0, system("Tool returned non-text content."));
        }
        parts
    };

    Message {
        role: "tool".to_string(),
        content,
        tool_call_id: Some(tool_result.tool_call_id.clone()),
        tool_calls: None,
    }
}

fn output_to_content_parts(output: &crate::wire::ToolOutput) -> Vec<ContentPart> {
    match output {
        crate::wire::ToolOutput::Text(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![ContentPart::Text { text: text.clone() }]
            }
        }
        crate::wire::ToolOutput::Parts(parts) => parts.clone(),
    }
}

pub fn check_message(
    message: &Message,
    capabilities: &HashSet<ModelCapability>,
) -> HashSet<ModelCapability> {
    let mut needed = HashSet::new();
    for part in &message.content {
        match part {
            ContentPart::ImageUrl { .. } => {
                needed.insert(ModelCapability::ImageIn);
            }
            ContentPart::VideoUrl { .. } => {
                needed.insert(ModelCapability::VideoIn);
            }
            ContentPart::Think { .. } => {
                needed.insert(ModelCapability::Thinking);
            }
            _ => {}
        }
    }
    needed.difference(capabilities).cloned().collect()
}

/// Merge adjacent user messages to produce a clean API input sequence.
///
/// Some LLM providers do not allow consecutive user messages. Dynamic
/// injections (plan-mode reminders, notifications) are stored as standalone
/// user messages in history; normalization merges them into the adjacent
/// user message.
///
/// Only `user` role messages are merged. Assistant and tool messages are
/// never merged because their `tool_calls` / `tool_call_id` fields form
/// linked pairs that must stay intact.
pub fn normalize_history(history: &[Message]) -> Vec<Message> {
    if history.is_empty() {
        return Vec::new();
    }

    let mut result: Vec<Message> = Vec::new();
    for msg in history {
        if let Some(last) = result.last_mut()
            && last.role == msg.role
            && msg.role == "user"
        {
            // Merge adjacent user messages by concatenating content.
            // TODO: skip merging if either message is a notification message
            // (see Python `is_notification_message`).
            last.content.extend(msg.content.clone());
            continue;
        }
        result.push(msg.clone());
    }
    result
}
