use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::constant;
use crate::telemetry::transport::AsyncTransport;

const DEFAULT_FLUSH_INTERVAL_S: f64 = 30.0;
const DEFAULT_FLUSH_THRESHOLD: usize = 50;

/// Buffers telemetry events and flushes them in batches.
#[derive(Clone)]
pub struct EventSink {
    transport: Arc<AsyncTransport>,
    buffer: Arc<Mutex<Vec<serde_json::Map<String, Value>>>>,
    context: serde_json::Map<String, Value>,
    model: String,
    ui_mode: String,
    flush_threshold: usize,
    flush_interval_secs: f64,
    flush_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl EventSink {
    pub fn new(transport: AsyncTransport, version: String, model: String, ui_mode: String) -> Self {
        let mut context = serde_json::Map::new();
        context.insert(
            "app_name".to_string(),
            Value::String(constant::NAME.to_string()),
        );
        context.insert(
            "version".to_string(),
            Value::String(if version.is_empty() {
                constant::get_version().to_string()
            } else {
                version
            }),
        );
        context.insert("runtime".to_string(), Value::String("rust".to_string()));
        context.insert(
            "platform".to_string(),
            Value::String(std::env::consts::OS.to_string()),
        );
        context.insert(
            "arch".to_string(),
            Value::String(std::env::consts::ARCH.to_string()),
        );
        context.insert("ci".to_string(), Value::Bool(std::env::var("CI").is_ok()));
        context.insert(
            "terminal".to_string(),
            Value::String(std::env::var("TERM_PROGRAM").unwrap_or_default()),
        );

        Self {
            transport: Arc::new(transport),
            buffer: Arc::new(Mutex::new(Vec::new())),
            context,
            model,
            ui_mode,
            flush_threshold: DEFAULT_FLUSH_THRESHOLD,
            flush_interval_secs: DEFAULT_FLUSH_INTERVAL_S,
            flush_task: Arc::new(Mutex::new(None)),
        }
    }

    /// Accept an event into the buffer. Non-blocking, thread-safe.
    pub fn accept(&self, event: crate::telemetry::EventRecord) {
        let enriched = self.enrich(event);

        let should_flush = {
            let mut buf = self.buffer.lock().unwrap();
            buf.push(enriched);
            buf.len() >= self.flush_threshold
        };

        if should_flush && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let sink = self.clone();
            drop(handle.spawn(async move {
                sink.flush().await;
            }));
        }
    }

    /// Start a background task that flushes every `flush_interval_secs`.
    pub fn start_periodic_flush(&self) {
        let mut task = self.flush_task.lock().unwrap();
        if task.is_some() {
            return;
        }
        let sink = self.clone();
        let interval_secs = self.flush_interval_secs;
        let handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_secs_f64(interval_secs);
            loop {
                tokio::time::sleep(interval).await;
                sink.flush().await;
            }
        });
        *task = Some(handle);
    }

    /// Cancel the periodic flush task.
    pub fn stop_periodic_flush(&self) {
        let mut task = self.flush_task.lock().unwrap();
        if let Some(handle) = task.take() {
            handle.abort();
        }
    }

    /// Retry sending any events that were previously saved to disk.
    pub async fn retry_disk_events(&self) {
        self.transport.retry_disk_events().await;
    }

    /// Discard all buffered events without sending them.
    pub fn clear_buffer(&self) {
        let mut buf = self.buffer.lock().unwrap();
        buf.clear();
    }

    /// Async flush: send all buffered events.
    pub async fn flush(&self) {
        let events = {
            let mut buf = self.buffer.lock().unwrap();
            if buf.is_empty() {
                return;
            }
            std::mem::take(&mut *buf)
        };
        self.transport.send(events).await;
    }

    /// Synchronous flush for exit / signal handlers.
    ///
    /// Writes remaining events to disk fallback file so they can be
    /// retried on next startup. Does NOT attempt HTTP.
    pub fn flush_sync(&self) {
        let events = {
            let mut buf = self.buffer.lock().unwrap();
            if buf.is_empty() {
                return;
            }
            std::mem::take(&mut *buf)
        };
        self.transport.save_to_disk(&events);
    }

    fn enrich(&self, record: crate::telemetry::EventRecord) -> serde_json::Map<String, Value> {
        let mut map = match serde_json::to_value(&record) {
            Ok(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };

        let mut ctx = self.context.clone();
        ctx.insert("ui_mode".to_string(), Value::String(self.ui_mode.clone()));
        if !self.model.is_empty() {
            ctx.insert("model".to_string(), Value::String(self.model.clone()));
        }
        map.insert("context".to_string(), Value::Object(ctx));
        map
    }
}
