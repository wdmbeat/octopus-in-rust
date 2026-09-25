use regex::Regex;

use crate::notifications::NotificationView;
use crate::wire::{ContentPart, Message};

lazy_static::lazy_static! {
    static ref NOTIFICATION_ID_RE: Regex = Regex::new(r#"<notification id="([^"]+)""#).unwrap();
}

/// Build a user message that injects a notification into the LLM context.
pub fn build_notification_message(view: &NotificationView) -> Message {
    let event = &view.event;
    let lines = [
        format!(
            r#"<notification id="{}" category="{}" type="{}" source_kind="{}" source_id="{}">"#,
            event.id, event.category, event.event_type, event.source_kind, event.source_id
        ),
        format!("Title: {}", event.title),
        format!("Severity: {}", event.severity),
        event.body.clone(),
        "</notification>".to_string(),
    ];

    Message {
        role: "user".to_string(),
        content: vec![ContentPart::Text {
            text: lines.join("\n"),
        }],
        tool_call_id: None,
        tool_calls: None,
    }
}

/// Extract notification IDs from user messages in history.
pub fn extract_notification_ids(history: &[Message]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for msg in history {
        if msg.role != "user" {
            continue;
        }
        for part in &msg.content {
            if let ContentPart::Text { text } = part {
                for cap in NOTIFICATION_ID_RE.captures_iter(text) {
                    if let Some(m) = cap.get(1) {
                        ids.insert(m.as_str().to_string());
                    }
                }
            }
        }
    }
    ids
}

/// Check whether a message is a notification injection message.
pub fn is_notification_message(msg: &Message) -> bool {
    if msg.role != "user" || msg.content.len() != 1 {
        return false;
    }
    if let ContentPart::Text { text } = &msg.content[0] {
        text.trim_start().starts_with("<notification ")
    } else {
        false
    }
}
