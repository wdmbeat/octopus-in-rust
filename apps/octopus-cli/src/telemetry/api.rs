use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

use crate::telemetry::sink::EventSink;

// ---------------------------------------------------------------------------
// Module-level state (zero dependencies on other octopus-cli modules)
// ---------------------------------------------------------------------------

const MAX_QUEUE_SIZE: usize = 1000;

struct TelemetryState {
    disabled: bool,
    device_id: Option<String>,
    session_id: Option<String>,
    client_info: Option<(String, Option<String>)>,
    sink: Option<EventSink>,
    queue: Vec<EventRecord>,
    session_started: HashSet<String>,
}

impl TelemetryState {
    fn new() -> Self {
        Self {
            disabled: false,
            device_id: None,
            session_id: None,
            client_info: None,
            sink: None,
            queue: Vec::new(),
            session_started: HashSet::new(),
        }
    }
}

static STATE: OnceLock<Mutex<TelemetryState>> = OnceLock::new();

fn state() -> &'static Mutex<TelemetryState> {
    STATE.get_or_init(|| Mutex::new(TelemetryState::new()))
}

// ---------------------------------------------------------------------------
// Event record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub event: String,
    pub timestamp: f64,
    pub properties: serde_json::Map<String, Value>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set device and session identifiers. Call once after app init.
pub fn set_context(device_id: String, session_id: String) {
    let mut state = state().lock().unwrap();
    state.device_id = Some(device_id);
    state.session_id = Some(session_id);
}

/// Set the wire/acp client name and version.
pub fn set_client_info(name: String, version: Option<String>) {
    let mut state = state().lock().unwrap();
    if name.is_empty() {
        return;
    }
    state.client_info = Some((name, version));
}

/// Return the current (name, version) tuple, or None if unset.
pub fn get_client_info() -> Option<(String, Option<String>)> {
    let state = state().lock().unwrap();
    state.client_info.clone()
}

/// Emit one session_started event for the current session in this process.
pub fn track_session_started_once(ui_mode: &str, resumed: bool) {
    let mut state = state().lock().unwrap();
    let session_id = match state.session_id.as_ref() {
        Some(id) => id.clone(),
        None => return,
    };
    if state.session_started.contains(&session_id) {
        return;
    }

    let ui = ui_mode.trim().to_lowercase();
    let (mut name, mut version) = (None, None);
    if let Some((n, v)) = state.client_info.clone() {
        name = Some(n);
        version = v;
    }
    if name.is_none() {
        name = Some(ui.clone());
    }

    state.session_started.insert(session_id.clone());
    drop(state);

    track_event("session_started", {
        let mut props = serde_json::Map::new();
        props.insert(
            "client_name".to_string(),
            Value::String(name.unwrap_or_else(|| "unknown".to_string())),
        );
        if let Some(v) = version {
            props.insert("client_version".to_string(), Value::String(v));
        }
        props.insert("ui_mode".to_string(), Value::String(ui));
        props.insert("resumed".to_string(), Value::Bool(resumed));
        props
    });

    // Best-effort immediate flush
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        drop(handle.spawn(async {
            flush().await;
        }));
    }
}

/// Permanently disable telemetry for this process. Events are silently dropped.
pub fn disable() {
    let mut state = state().lock().unwrap();
    state.disabled = true;
    state.queue.clear();
    if let Some(ref sink) = state.sink {
        sink.clear_buffer();
    }
}

/// Attach the event sink and drain any queued events.
pub fn attach_sink(sink: EventSink) {
    let mut state = state().lock().unwrap();

    // Flush old sink synchronously before replacing
    if let Some(ref old) = state.sink {
        old.flush_sync();
    }

    // Backfill device_id/session_id for queued events
    if !state.queue.is_empty() {
        let device_id = state.device_id.clone();
        let session_id = state.session_id.clone();
        for mut event in state.queue.drain(..) {
            if event.device_id.is_none() {
                event.device_id = device_id.clone();
            }
            if event.session_id.is_none() {
                event.session_id = session_id.clone();
            }
            sink.accept(event);
        }
    }

    state.sink = Some(sink);
}

/// Record a telemetry event. Non-blocking.
pub fn track_event(event: &str, properties: serde_json::Map<String, Value>) {
    let mut state = state().lock().unwrap();
    if state.disabled {
        return;
    }

    let record = EventRecord {
        event_id: uuid::Uuid::new_v4().to_string(),
        device_id: state.device_id.clone(),
        session_id: state.session_id.clone(),
        event: event.to_string(),
        timestamp: chrono::Utc::now().timestamp() as f64,
        properties,
    };

    if let Some(ref sink) = state.sink {
        sink.accept(record);
    } else {
        if state.queue.len() < MAX_QUEUE_SIZE {
            state.queue.push(record);
        }
    }
}

/// Return the current sink, or None if not attached.
pub fn get_sink() -> Option<EventSink> {
    let state = state().lock().unwrap();
    state.sink.clone()
}

/// Asynchronously flush any buffered events.
pub async fn flush() {
    let sink = {
        let state = state().lock().unwrap();
        state.sink.clone()
    };
    if let Some(sink) = sink {
        sink.flush().await;
    }
}

/// Synchronously flush any buffered events. Called on exit.
pub fn flush_sync() {
    let sink = {
        let state = state().lock().unwrap();
        state.sink.clone()
    };
    if let Some(sink) = sink {
        sink.flush_sync();
    }
}

/// Load or create a persistent device ID.
pub fn get_or_create_device_id() -> String {
    let path = crate::share::get_share_dir().join("device_id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(&path, &id);
    id
}
