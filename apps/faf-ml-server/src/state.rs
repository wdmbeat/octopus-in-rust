//! Shared application state for Axum handlers.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use faf_blueprints::FafBlueprints;
use faf_ml_core::DatagenJob;
use uuid::Uuid;

use crate::error::{Error, Result};

/// State shared across all HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub data_dir: Arc<PathBuf>,
    pub assets_dir: Arc<PathBuf>,
    /// Strategic-icon sprite directory (`FAF_ML_ICONS_DIR`) for datagen jobs.
    pub icons_dir: Arc<PathBuf>,
    /// Unit portrait directory (`FAF_ML_PORTRAITS_DIR`) for the Units page.
    pub portraits_dir: Arc<PathBuf>,
    /// Unit blueprints backing `/api/units` (shared ETFreeman unit database).
    pub blueprints: Arc<FafBlueprints>,
    /// Live registry of the current/last training run (`/ws/training` +
    /// `GET /api/training/status`) — a handle to the training manager actor
    /// in `faf-ml-model`; `Idle` until the first run starts.
    pub training: faf_ml_model::manager::TrainManagerHandle,
    /// Display name per unit id (uppercase): the game nickname
    /// (`General.UnitName`, e.g. "Spook") when set, otherwise the ordinary
    /// description (e.g. "Spy Plane"). Loaded from the raw unit index so
    /// `faf-blueprints` stays untouched.
    pub unit_display_names: Arc<HashMap<String, String>>,
    /// In-memory datagen job registry (progress is polled, not streamed).
    pub jobs: Arc<Mutex<HashMap<Uuid, DatagenJob>>>,
    /// Parsed icon-set sources (`FAF_ML_ICON_MODS`, or the legacy flat
    /// icons dir as a builtin fallback), in override-priority order.
    pub icon_sets: Arc<Vec<crate::icon_sets::IconSet>>,
}

impl AppState {
    pub fn new(
        data_dir: PathBuf,
        assets_dir: PathBuf,
        icons_dir: PathBuf,
        portraits_dir: PathBuf,
        icon_mods: Vec<PathBuf>,
    ) -> Result<Self> {
        let unit_index = match std::env::var("FAFCN_UNITS_FILE") {
            Ok(path) => faf_units::FafUnitIndex::new(path.into()),
            Err(_) => faf_units::FafUnitIndex::load_default(),
        }
        .map_err(|e| Error::Internal(format!("loading unit index: {e:#}")))?;
        let unit_display_names: HashMap<String, String> = unit_index
            .units
            .iter()
            .map(|u| {
                let name = u
                    .general
                    .as_ref()
                    .and_then(|g| g.unit_name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| u.description.clone());
                (u.id.to_ascii_uppercase(), name)
            })
            .collect();

        let icon_sets = crate::icon_sets::load_icon_sets(&icon_mods, &icons_dir);
        let state = Self {
            data_dir: Arc::new(data_dir),
            assets_dir: Arc::new(assets_dir),
            icons_dir: Arc::new(icons_dir),
            portraits_dir: Arc::new(portraits_dir),
            blueprints: Arc::new(
                FafBlueprints::new()
                    .map_err(|e| Error::Internal(format!("loading unit blueprints: {e:#}")))?,
            ),
            unit_display_names: Arc::new(unit_display_names),
            // Spawns the manager actor on the ambient tokio runtime — only
            // valid because `AppState::new` is called from async main.
            training: faf_ml_model::manager::TrainManagerHandle::spawn(),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            icon_sets: Arc::new(icon_sets),
        };
        std::fs::create_dir_all(state.screenshots_dir())?;
        std::fs::create_dir_all(state.labels_dir())?;
        std::fs::create_dir_all(state.datasets_dir())?;
        Ok(state)
    }

    pub fn screenshots_dir(&self) -> PathBuf {
        self.data_dir.join("screenshots")
    }

    pub fn labels_dir(&self) -> PathBuf {
        self.data_dir.join("labels")
    }

    pub fn datasets_dir(&self) -> PathBuf {
        self.data_dir.join("datasets")
    }

    pub fn index_path(&self) -> PathBuf {
        self.screenshots_dir().join("index.json")
    }

    pub fn classes_path(&self) -> PathBuf {
        self.data_dir.join("classes.txt")
    }

    /// Strategic-icon ↔ unit mapping artifact (`faf-unit-tools icon-map`).
    pub fn icon_map_path(&self) -> PathBuf {
        self.data_dir.join("icon-map.json")
    }

    /// User-edited icon configuration (`/api/icons/config`).
    pub fn icon_config_path(&self) -> PathBuf {
        self.data_dir.join("icon-config.json")
    }

    pub fn image_path(&self, id: Uuid) -> PathBuf {
        self.screenshots_dir().join(format!("{id}.png"))
    }

    pub fn labels_path(&self, id: Uuid) -> PathBuf {
        self.labels_dir().join(format!("{id}.json"))
    }

    pub fn dataset_path(&self, name: &str) -> PathBuf {
        self.datasets_dir().join(format!("{name}.json"))
    }
}

/// Validate a dataset name so it is a safe file name (`<name>.json`).
pub fn valid_dataset_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::BadRequest(format!(
            "invalid dataset name {name:?}: only [A-Za-z0-9._-] allowed"
        )))
    }
}
