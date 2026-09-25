use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

const TELEMETRY_ENDPOINT: &str = "https://telemetry-logs.kimi.com/v1/event";
const SEND_TIMEOUT_SECS: u64 = 10;
const DISK_EVENT_MAX_AGE_S: u64 = 7 * 24 * 3600;
const RETRY_BACKOFFS_S: &[f64] = &[1.0, 4.0, 16.0];
const SERVER_EVENT_PREFIX: &str = "kfc_";
const USER_ID_PREFIX: &str = "kfc_device_id_";

/// Sends telemetry events over HTTP with disk fallback.
#[derive(Clone)]
pub struct AsyncTransport {
    device_id: String,
    get_access_token: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    endpoint: String,
    retry_backoffs: Vec<f64>,
    client: reqwest::Client,
    telemetry_dir: PathBuf,
}

#[derive(Debug)]
struct TransientError(String);

impl std::fmt::Display for TransientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TransientError {}

impl AsyncTransport {
    pub fn new(
        device_id: String,
        get_access_token: Arc<dyn Fn() -> Option<String> + Send + Sync>,
        telemetry_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(SEND_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();
        Self {
            device_id,
            get_access_token,
            endpoint: TELEMETRY_ENDPOINT.to_string(),
            retry_backoffs: RETRY_BACKOFFS_S.to_vec(),
            client,
            telemetry_dir,
        }
    }

    /// Send a batch of events with in-process retry, falling back to disk.
    pub async fn send(&self, events: Vec<serde_json::Map<String, Value>>) {
        if events.is_empty() {
            return;
        }

        let payload = match self.build_payload(&events) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "Telemetry payload schema violation, dropping {} events: {}",
                    events.len(),
                    e
                );
                return;
            }
        };

        for attempt_idx in 0..=self.retry_backoffs.len() {
            match self.send_http(&payload).await {
                Ok(()) => return,
                Err(TransientError(ref msg)) => {
                    if attempt_idx >= self.retry_backoffs.len() {
                        tracing::debug!(
                            "Telemetry send transient failure after {} attempts: {}",
                            attempt_idx + 1,
                            msg
                        );
                        break;
                    }
                    let backoff = self.retry_backoffs[attempt_idx];
                    tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
                }
            }
        }

        self.save_to_disk(&events);
    }

    async fn send_http(&self, payload: &Value) -> Result<(), TransientError> {
        let token = (self.get_access_token)();
        let mut req = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(payload);
        if let Some(ref t) = token {
            req = req.header("Authorization", format!("Bearer {}", t));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| TransientError(e.to_string()))?;
        let status = resp.status();

        if status == reqwest::StatusCode::UNAUTHORIZED && token.is_some() {
            // Retry without token (anonymous)
            let req = self
                .client
                .post(&self.endpoint)
                .header("Content-Type", "application/json")
                .json(payload);
            let resp = req
                .send()
                .await
                .map_err(|e| TransientError(e.to_string()))?;
            let status = resp.status();
            if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(TransientError(format!("HTTP {}", status)));
            }
            // Any other status (including 4xx) → drop
            return Ok(());
        }

        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(TransientError(format!("HTTP {}", status)));
        }

        // 4xx (except 429) or success → done
        Ok(())
    }

    fn build_payload(&self, events: &[serde_json::Map<String, Value>]) -> Result<Value, TypeError> {
        let mut flat_events = Vec::new();
        for event in events {
            flat_events.push(self.flatten_event(self.apply_prefix(event.clone())?)?);
        }
        Ok(serde_json::json!({
            "user_id": format!("{}{}", USER_ID_PREFIX, self.device_id),
            "events": flat_events,
        }))
    }

    fn apply_prefix(
        &self,
        mut event: serde_json::Map<String, Value>,
    ) -> Result<serde_json::Map<String, Value>, TypeError> {
        if let Some(Value::String(name)) = event.get("event")
            && !name.is_empty()
            && !name.starts_with(SERVER_EVENT_PREFIX)
        {
            event.insert(
                "event".to_string(),
                Value::String(format!("{}{}", SERVER_EVENT_PREFIX, name)),
            );
        }
        Ok(event)
    }

    fn flatten_event(
        &self,
        event: serde_json::Map<String, Value>,
    ) -> Result<serde_json::Map<String, Value>, TypeError> {
        let mut out = serde_json::Map::new();
        for (key, value) in event {
            if key == "properties" {
                if let Some(props) = value.as_object() {
                    for (pk, pv) in props {
                        Self::assert_primitive("property", pk, pv)?;
                        out.insert(format!("property_{}", pk), pv.clone());
                    }
                }
            } else if key == "context" {
                if let Some(ctx) = value.as_object() {
                    for (ck, cv) in ctx {
                        Self::assert_primitive("context", ck, cv)?;
                        out.insert(format!("context_{}", ck), cv.clone());
                    }
                }
            } else {
                out.insert(key, value);
            }
        }
        Ok(out)
    }

    fn assert_primitive(scope: &str, key: &str, value: &Value) -> Result<(), TypeError> {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
            _ => Err(TypeError(format!(
                "telemetry {}.{} must be primitive, got {}",
                scope, key, value
            ))),
        }
    }

    /// Persist events to disk for later retry. Append-only JSONL.
    pub fn save_to_disk(&self, events: &[serde_json::Map<String, Value>]) {
        if events.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&self.telemetry_dir);
        let file_name = format!("failed_{}.jsonl", &uuid::Uuid::new_v4().to_string()[..12]);
        let path = self.telemetry_dir.join(file_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
        {
            Ok(mut file) => {
                for event in events {
                    if let Ok(json) = serde_json::to_string(event) {
                        let _ = writeln!(file, "{}", json);
                    }
                }
                tracing::debug!(
                    "Saved {} telemetry events to {}",
                    events.len(),
                    path.display()
                );
            }
            Err(_) => {
                tracing::debug!("Failed to save telemetry events to disk");
            }
        }
    }

    /// On startup, scan disk for persisted events and resend them.
    pub async fn retry_disk_events(&self) {
        let entries = match std::fs::read_dir(&self.telemetry_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for entry in entries.flatten() {
            let path = entry.path();
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !fname.starts_with("failed_") || !fname.ends_with(".jsonl") {
                continue;
            }

            // Delete expired files
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
            {
                let mtime = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now.saturating_sub(mtime) > DISK_EVENT_MAX_AGE_S {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            }

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut events = Vec::new();
            let mut corrupted = false;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Map<String, Value>>(line) {
                    Ok(event) => events.push(event),
                    Err(_) => {
                        corrupted = true;
                        break;
                    }
                }
            }

            if corrupted || events.is_empty() {
                let _ = std::fs::remove_file(&path);
                continue;
            }

            match self.build_payload(&events) {
                Ok(payload) => match self.send_http(&payload).await {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&path);
                        tracing::debug!(
                            "Retried {} telemetry events from {}",
                            events.len(),
                            path.display()
                        );
                    }
                    Err(TransientError(_)) => {
                        tracing::debug!("Retry of {} failed, will try again later", path.display());
                    }
                },
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

#[derive(Debug)]
struct TypeError(String);

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
