//! Training-monitor wire types: the `/ws/training` protocol shared by
//! `faf-ml-server` and `faf-ml-web` (JSON text frames, externally-tagged
//! enums — same style as `faf-sim-protocol`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Training-run parameters (sent by the Training page / MCP start tool).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// Dataset snapshot to train on (`datasets/<name>.json`). REQUIRED:
    /// training consumes an immutable snapshot, never the live store —
    /// create one on the Datasets page first (workflow step 5).
    #[serde(default)]
    pub dataset: String,
    /// Number of epochs to run.
    #[serde(default = "default_epochs")]
    pub epochs: usize,
    /// Batch size (hard-capped at 4 by the GPU buffer limit for the real
    /// detector; the dummy service accepts anything).
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Learning rate.
    #[serde(default = "default_lr")]
    pub lr: f64,
    /// Fraction of samples held out for validation (epoch-end eval).
    #[serde(default = "default_valid_fraction")]
    pub valid_fraction: f32,
    /// Cap optimizer steps per epoch (smoke runs; `None` = full epochs).
    #[serde(default)]
    pub max_batches: Option<usize>,
    /// Use the portable CPU (NdArray) backend instead of Wgpu/Vulkan.
    #[serde(default)]
    pub cpu: bool,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            dataset: String::new(),
            epochs: default_epochs(),
            batch_size: default_batch_size(),
            lr: default_lr(),
            valid_fraction: default_valid_fraction(),
            max_batches: None,
            cpu: false,
        }
    }
}

fn default_epochs() -> usize {
    50
}
fn default_batch_size() -> usize {
    4
}
fn default_lr() -> f64 {
    1e-3
}
fn default_valid_fraction() -> f32 {
    0.1
}

/// One chart point, emitted per training batch (epoch-eval fields set only
/// on the epoch-end point).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingMetricsPoint {
    /// Monotonic sequence number (x axis + dedup).
    pub seq: u64,
    pub epoch: usize,
    /// Batch within the epoch (1-based).
    pub batch: usize,
    /// Total batches of the whole run (for the progress line).
    pub total_batches: usize,
    pub train_loss: f64,
    pub cls_loss: f64,
    pub bbox_loss: f64,
    /// Validation loss (epoch-end points only).
    #[serde(default)]
    pub valid_loss: Option<f64>,
    /// Detection mAP (epoch-end points only; dummy until real eval exists).
    #[serde(default)]
    pub map: Option<f64>,
}
/// Uniform Msg used by web client, server for trainning a model.
/// Intraction between clients and model trainning in backend are like chat networking.
/// The trainning task is like a client which passively receive msg and/or send back msg from a channel.
/// The trainning manager (server) use tokio select! to choose to receive msg either from a trainning task or
/// from control command from that task's associated frontend.
/// When frontend initialize a training task, it use direct call to train manager. Manager accept its connection
/// and return a handler just like a TCP server handle a client connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TrainningMsg {
    Start {
        config: TrainingConfig,
        /// Post-batch throttle in batches/sec (≤ 0 = unlimited).
        speed: f64,
    },
    Pause(Uuid),
    Resume(Uuid),
    Stop(Uuid),
    Reset(Uuid),
    SetSpeed {
        id: Uuid,
        batches_per_sec: f64,
    },
}

/// Browser → server messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrainingClientMessage {
    /// Start a training run (must be the first message on the socket).
    Start {
        config: TrainingConfig,
        /// Post-batch throttle in batches/sec (≤ 0 = unlimited).
        speed: f64,
    },
    /// Attach as a viewer to the currently active run (replay + live stream;
    /// does not start anything).
    Attach,
    /// Runtime command for a running job.
    Command(TrainingCommand),
}

/// Runtime commands for a training run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum TrainingCommand {
    Pause,
    Resume,
    /// Finish the current batch, save the checkpoint, end the run (the run
    /// record stays visible).
    Stop,
    /// Like Stop (checkpoint still saved), but additionally wipes the run
    /// record and tells every viewer to clear its charts.
    Reset,
    SetSpeed {
        batches_per_sec: f64,
    },
}

/// Lifecycle of one training run. `Pausing`/`Stopping` are the instant
/// command acknowledgments; the settled `Paused`/`Stopped` arrive when the
/// training thread reaches the batch boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TrainingStatus {
    Running,
    Pausing,
    Paused,
    Stopping,
    Done {
        duration_secs: u64,
    },
    /// Ended via Stop or Reset (checkpoint saved).
    Stopped {
        duration_secs: u64,
    },
    Failed {
        error: String,
    },
}

/// Final outcome of a training run (`GET /api/training/status`). A Reset
/// wipes the run record entirely — it leaves no result behind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TrainingRunResult {
    Done { run_dir: String, duration_secs: u64 },
    Stopped { run_dir: String, duration_secs: u64 },
    Failed { error: String },
}

/// `GET /api/training/status` response: the current or most recent training
/// run (the web page renders this on load; the WS streams live updates).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingRunStatus {
    pub config: TrainingConfig,
    pub started_at: DateTime<Utc>,
    pub status: TrainingStatus,
    /// Metrics points produced so far.
    pub points: usize,
    pub latest: Option<TrainingMetricsPoint>,
    #[serde(default)]
    pub result: Option<TrainingRunResult>,
}

/// One checkpoint run directory (`GET /api/runs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    /// Directory name (timestamp, e.g. `20260910-123045`).
    pub name: String,
    /// Number of classes the model detects.
    pub classes: usize,
}

/// `POST /api/predict(/annotate)` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictRequest {
    /// Run directory name under `runs/` (no path separators).
    pub run: String,
    /// Store screenshot to run the detector on.
    pub image_id: uuid::Uuid,
    /// Minimum class score to keep a detection (default 0.3).
    #[serde(default)]
    pub score_threshold: Option<f32>,
    /// Use the portable CPU (NdArray) backend instead of Wgpu/Vulkan.
    #[serde(default)]
    pub cpu: bool,
}

/// One detection, in absolute pixels of the model input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionView {
    pub class: String,
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// `POST /api/predict` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictResponse {
    pub run: String,
    pub image_id: uuid::Uuid,
    pub detections: Vec<DetectionView>,
}

/// Server → browser messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrainingServerMessage {
    Metrics(TrainingMetricsPoint),
    Status(TrainingStatus),
    Error(String),
    /// The run record was wiped (`TrainingCommand::Reset`); viewers clear
    /// their charts and go back to idle.
    Reset,
    /// Training thread exited cleanly (after a terminal `Status`); the server
    /// closes the socket right after.
    Finished,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_fill_missing_fields() {
        let config: TrainingConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, TrainingConfig::default());
        assert_eq!(config.batch_size, 4);
    }

    #[test]
    fn protocol_round_trips() {
        let msg = TrainingServerMessage::Metrics(TrainingMetricsPoint {
            seq: 7,
            epoch: 1,
            batch: 3,
            total_batches: 100,
            train_loss: 0.5,
            cls_loss: 0.3,
            bbox_loss: 0.2,
            valid_loss: None,
            map: Some(0.42),
        });
        let raw = serde_json::to_string(&msg).unwrap();
        let back: TrainingServerMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, msg);

        let cmd = TrainingClientMessage::Command(TrainingCommand::SetSpeed {
            batches_per_sec: 5.0,
        });
        let raw = serde_json::to_string(&cmd).unwrap();
        assert_eq!(
            raw,
            r#"{"type":"command","command":"set_speed","batches_per_sec":5.0}"#
        );
        let back: TrainingClientMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, cmd);

        let cmd = TrainingClientMessage::Command(TrainingCommand::Reset);
        let raw = serde_json::to_string(&cmd).unwrap();
        assert_eq!(raw, r#"{"type":"command","command":"reset"}"#);
        let back: TrainingClientMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, cmd);

        let status = TrainingServerMessage::Status(TrainingStatus::Stopped { duration_secs: 42 });
        let raw = serde_json::to_string(&status).unwrap();
        let back: TrainingServerMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, status);

        let reset = TrainingServerMessage::Reset;
        let raw = serde_json::to_string(&reset).unwrap();
        assert_eq!(raw, r#"{"type":"reset"}"#);
        let back: TrainingServerMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, reset);
    }
}
