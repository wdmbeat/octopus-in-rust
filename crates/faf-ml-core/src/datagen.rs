//! Datagen wire types: the generation config (`POST /api/datagen` body) and
//! the job registry entries the web UI polls while generation runs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Synthetic-data generation parameters.
///
/// Accepted as JSON by `POST /api/datagen` and stored on the job for
/// reproducibility. Consumed by `faf-ml-datagen::generate` (re-exported
/// there as `faf_ml_datagen::DatagenConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatagenConfig {
    /// Number of synthetic samples to generate.
    #[serde(default = "default_count")]
    pub count: usize,
    /// Side length of the square crop each sample is generated on.
    #[serde(default = "default_size")]
    pub size: u32,
    /// Max units pasted per sample (min is 1).
    #[serde(default = "default_max_units")]
    pub max_units: usize,
    /// Sprite scale range as a fraction of the 36×40 source (0.35 ≈ 13 px —
    /// the zoomed-out on-screen size; check against real screenshots!).
    #[serde(default = "default_scale_min")]
    pub scale_min: f32,
    #[serde(default = "default_scale_max")]
    pub scale_max: f32,
    /// RNG seed (fixed by default for reproducible datasets).
    #[serde(default = "default_seed")]
    pub seed: u64,
}

impl Default for DatagenConfig {
    fn default() -> Self {
        Self {
            count: default_count(),
            size: default_size(),
            max_units: default_max_units(),
            scale_min: default_scale_min(),
            scale_max: default_scale_max(),
            seed: default_seed(),
        }
    }
}

fn default_count() -> usize {
    200
}
fn default_size() -> u32 {
    640
}
fn default_max_units() -> usize {
    25
}
fn default_scale_min() -> f32 {
    0.35
}
fn default_scale_max() -> f32 {
    0.65
}
fn default_seed() -> u64 {
    42
}

/// One datagen background job (in-memory registry, lost on restart).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatagenJob {
    pub id: Uuid,
    /// The config the job was started with (reproducibility).
    pub config: DatagenConfig,
    pub started_at: DateTime<Utc>,
    pub status: DatagenStatus,
}

/// Job lifecycle, enum-with-data per octopus style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DatagenStatus {
    /// Generation in progress: `done` of `total` samples stored.
    Running { done: usize, total: usize },
    /// Finished: `generated` samples were stored (as `synthetic`
    /// screenshots with labels).
    Done { generated: usize },
    /// Aborted with an error message.
    Failed { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_fill_missing_fields() {
        let config: DatagenConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, DatagenConfig::default());
        assert_eq!(config.size, 640);
    }

    #[test]
    fn status_round_trips_as_tagged_json() {
        let status = DatagenStatus::Running { done: 1, total: 3 };
        let raw = serde_json::to_string(&status).unwrap();
        assert_eq!(raw, r#"{"state":"running","done":1,"total":3}"#);
        let back: DatagenStatus = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, status);
    }
}
