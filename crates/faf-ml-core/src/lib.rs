//! Shared types for the faf-ml platform: screenshot metadata, bounding-box
//! labels, dataset snapshots, datagen jobs, and YOLO line conversion helpers.
//!
//! Both `faf-ml-server` and `faf-ml-web` depend on this crate so the wire
//! format and the on-disk JSON layout stay in sync.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod datagen;
mod icons;
mod training;

pub use datagen::{DatagenConfig, DatagenJob, DatagenStatus};
pub use icons::{IconClassInfo, IconConfig, IconSetInfo, UnitIconEffective};
pub use training::{
    DetectionView, PredictRequest, PredictResponse, RunInfo, TrainingCommand, TrainingConfig,
    TrainingEvent, TrainingMetricsPoint, TrainingRunResult, TrainingRunStatus, TrainingStatus,
};

/// What a screenshot is FOR in the training-data pipeline.
///
/// The distinction matters because the two kinds have opposite jobs:
/// `Background` images (empty terrain) are the compositing canvas for
/// faf-ml-datagen; `Battle` images (real units) are the held-out test /
/// correction pool and must never be composited on or trained against
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotKind {
    /// Uploaded but not yet triaged. The safe default: excluded from every
    /// pool until a human marks it (a misclassified battle shot in the
    /// background pool would poison datagen with unlabeled real units).
    #[default]
    Unclassified,
    /// Real battle frame with units — held-out test/correction pool.
    Battle,
    /// Empty terrain — datagen's background pool.
    Background,
    /// Datagen output (synthetic, auto-labeled).
    Synthetic,
}

impl ScreenshotKind {
    /// Query-string / CLI spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Battle => "battle",
            Self::Background => "background",
            Self::Synthetic => "synthetic",
        }
    }
}

impl fmt::Display for ScreenshotKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One uploaded screenshot's metadata (stored in `screenshots/index.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotMeta {
    pub id: Uuid,
    /// Original file name as uploaded (kept for display only).
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub uploaded_at: DateTime<Utc>,
    /// Pipeline role; defaults to `unclassified` ("needs triage") for
    /// pre-existing index entries.
    #[serde(default)]
    pub kind: ScreenshotKind,
    /// The datagen job that produced this sample (`None` for uploads and
    /// for samples generated before job tracking existed).
    #[serde(default)]
    pub job_id: Option<Uuid>,
}

/// `icon-map.json`: strategic-icon class ↔ unit id mapping (many-to-many).
///
/// Lives OUTSIDE `faf-blueprints` on purpose: the blueprints stay a pure
/// read-only view of the game database, while icon sets (custom strategic
/// icons the ML pipeline trains on) are maintained separately. Regenerate
/// with `faf-unit-tools icon-map`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IconMap {
    /// icon class (no `icon_` prefix, e.g. `fighter3_intel`) → unit ids
    /// sharing that icon. One unit may appear under several classes
    /// (multiple icon sets), one class may cover several units.
    pub mapping: std::collections::HashMap<String, Vec<String>>,
    /// Icon classes no unit maps to (custom-set extras).
    #[serde(default)]
    pub orphan_icons: Vec<String>,
    /// Blueprint `strategic_icon_name`s not covered by the custom icon set.
    #[serde(default)]
    pub uncovered_icon_names: Vec<String>,
}

/// A unit sharing an icon class (id + display name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedUnit {
    pub id: String,
    pub name: String,
}

/// One icon class mapped to a unit, plus every unit sharing that class
/// (four FAF factions only — mod factions like Nomads are excluded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconClassMapping {
    pub class: String,
    pub units: Vec<SharedUnit>,
}

/// `GET /api/units/:id/icons` response: a unit's blueprint default icon and
/// every custom-set icon class mapped to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitIcons {
    pub unit_id: String,
    /// The unit database's own `strategic_icon_name` (with `icon_` prefix).
    pub blueprint_icon: Option<String>,
    pub mapped_icons: Vec<IconClassMapping>,
}

/// One labeled bounding box, in **absolute pixel** coordinates of the
/// full-size image (`x`, `y` = top-left corner). The web UI scales these by
/// the displayed-vs-natural image ratio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledBox {
    /// Class name (must exist in `classes.txt`).
    pub class: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Immutable dataset snapshot (stored in `datasets/<name>.json`). Labels are
/// embedded so later label edits never mutate a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub entries: Vec<DatasetEntry>,
}

/// One image's contribution to a dataset snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetEntry {
    pub image_id: Uuid,
    pub labels: Vec<LabeledBox>,
}

/// Error returned when parsing a YOLO label line fails.
#[derive(Debug, Clone, PartialEq)]
pub enum YoloError {
    /// Line does not have exactly 5 whitespace-separated fields.
    FieldCount,
    /// Class id field is not a valid non-negative integer.
    BadClassId,
    /// Class id has no entry in the provided class list.
    UnknownClass(usize),
    /// A coordinate field is not a valid float.
    BadCoordinate,
}

impl fmt::Display for YoloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            YoloError::FieldCount => write!(f, "expected 5 fields: <class> <cx> <cy> <w> <h>"),
            YoloError::BadClassId => write!(f, "invalid class id"),
            YoloError::UnknownClass(id) => write!(f, "class id {id} not in class list"),
            YoloError::BadCoordinate => write!(f, "invalid coordinate"),
        }
    }
}

impl std::error::Error for YoloError {}

impl LabeledBox {
    /// Convert to a YOLO-format line: `<class_id> <cx> <cy> <w> <h>` with all
    /// values normalized to 0..=1 relative to the image size.
    pub fn to_yolo(&self, classes: &[String], image_width: u32, image_height: u32) -> String {
        let class_id = classes.iter().position(|c| c == &self.class).unwrap_or(0);
        let cx = (self.x + self.w / 2.0) / image_width as f32;
        let cy = (self.y + self.h / 2.0) / image_height as f32;
        let w = self.w / image_width as f32;
        let h = self.h / image_height as f32;
        format!("{class_id} {cx:.6} {cy:.6} {w:.6} {h:.6}")
    }

    /// Parse a YOLO-format line back into absolute pixel coordinates.
    ///
    /// `classes` maps class ids to names (line number = class id, matching
    /// the `classes.txt` convention).
    pub fn from_yolo(
        line: &str,
        classes: &[String],
        image_width: u32,
        image_height: u32,
    ) -> Result<Self, YoloError> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(YoloError::FieldCount);
        }
        let class_id: usize = fields[0].parse().map_err(|_| YoloError::BadClassId)?;
        let class = classes
            .get(class_id)
            .ok_or(YoloError::UnknownClass(class_id))?
            .clone();
        let coord = |s: &str| s.parse::<f32>().map_err(|_| YoloError::BadCoordinate);
        let cx = coord(fields[1])?;
        let cy = coord(fields[2])?;
        let w = coord(fields[3])? * image_width as f32;
        let h = coord(fields[4])? * image_height as f32;
        Ok(LabeledBox {
            class,
            x: cx * image_width as f32 - w / 2.0,
            y: cy * image_height as f32 - h / 2.0,
            w,
            h,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes() -> Vec<String> {
        vec!["tank".to_string(), "bomber".to_string()]
    }

    #[test]
    fn yolo_round_trip() {
        let b = LabeledBox {
            class: "bomber".to_string(),
            x: 100.0,
            y: 50.0,
            w: 40.0,
            h: 20.0,
        };
        let line = b.to_yolo(&classes(), 640, 640);
        let back = LabeledBox::from_yolo(&line, &classes(), 640, 640).unwrap();
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert_eq!(back.class, "bomber");
        assert!(close(back.x, b.x) && close(back.y, b.y));
        assert!(close(back.w, b.w) && close(back.h, b.h));
    }

    #[test]
    fn yolo_rejects_bad_lines() {
        assert_eq!(
            LabeledBox::from_yolo("1 0.5 0.5 0.1", &classes(), 640, 640).unwrap_err(),
            YoloError::FieldCount
        );
        assert_eq!(
            LabeledBox::from_yolo("9 0.5 0.5 0.1 0.1", &classes(), 640, 640).unwrap_err(),
            YoloError::UnknownClass(9)
        );
    }
}
