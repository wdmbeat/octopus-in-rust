pub mod llm;
pub mod manager;
pub mod store;
pub mod wire;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type NotificationCategory = String;
pub type NotificationSeverity = String;
pub type NotificationSink = String;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "status", content = "at")]
#[derive(Default)]
pub enum NotificationDeliveryStatus {
    #[default]
    Pending,
    Claimed(f64),
    Acked(f64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEvent {
    pub version: i32,
    pub id: String,
    pub category: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub source_kind: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub payload: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
}

impl NotificationEvent {
    pub fn new(
        id: String,
        category: impl Into<String>,
        event_type: impl Into<String>,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            version: 1,
            id,
            category: category.into(),
            event_type: event_type.into(),
            source_kind: source_kind.into(),
            source_id: source_id.into(),
            title: title.into(),
            body: body.into(),
            severity: "info".to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            payload: HashMap::new(),
            targets: vec!["llm".to_string(), "wire".to_string(), "shell".to_string()],
            dedupe_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationSinkState {
    pub status: NotificationDeliveryStatus,
}

impl NotificationSinkState {
    pub fn pending() -> Self {
        Self {
            status: NotificationDeliveryStatus::Pending,
        }
    }

    pub fn claimed(at: f64) -> Self {
        Self {
            status: NotificationDeliveryStatus::Claimed(at),
        }
    }

    pub fn acked(at: f64) -> Self {
        Self {
            status: NotificationDeliveryStatus::Acked(at),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationDelivery {
    pub sinks: HashMap<String, NotificationSinkState>,
}

#[derive(Debug, Clone)]
pub struct NotificationView {
    pub event: NotificationEvent,
    pub delivery: NotificationDelivery,
}
