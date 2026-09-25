//! Server-wide configuration loaded from environment variables.

use std::path::PathBuf;

/// Return the workspace root directory (`octopus/`).
///
/// The path is derived from the compile-time crate location; on deployed
/// hosts (where the repo does not exist) canonicalization fails, in which
/// case the unresolved path is returned — every consumer is expected to be
/// overridden via `FAF_ML_*` environment variables there.
pub fn workspace_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    root.canonicalize().unwrap_or(root)
}

/// HTTP server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Port to bind the HTTP server on.
    pub port: u16,

    /// Root of the ML data store (screenshots, labels, datasets, classes).
    pub data_dir: PathBuf,

    /// Directory containing the built Dioxus web assets.
    pub assets_dir: PathBuf,

    /// Directory containing the strategic-icon `.dds` sprites datagen
    /// composites onto background screenshots.
    pub icons_dir: PathBuf,

    /// Directory containing unit portrait PNGs (`{UNIT_ID}.png`) served by
    /// `/api/portraits/:id` for the Units page.
    pub portraits_dir: PathBuf,

    /// Directories of icon-set mods (FAF strategic-icon mods with
    /// `mod_info.lua` + `mod_icons.lua` + `custom-strategic-icons/`), in
    /// override-priority order. Parsed once at startup; the Icons page and
    /// datagen resolve sprites/assignments from the enabled ones.
    pub icon_mods: Vec<PathBuf>,
}

impl ServerConfig {
    /// Load configuration from environment variables.
    ///
    /// Variables:
    /// - `FAF_ML_PORT` — bind port (default: `3100`).
    /// - `FAF_ML_DATA_DIR` — data store root (default: `data/faf-ml`).
    /// - `FAF_ML_WEB_DIST` — built web assets directory (default:
    ///   `target/dx/faf-ml-web/release/web/public`).
    /// - `FAF_ML_ICONS_DIR` — strategic-icon sprite directory for datagen
    ///   (default: `tmp/custom-strategic-icons`).
    /// - `FAF_ML_PORTRAITS_DIR` — unit portrait directory for the Units page
    ///   (default: `assets/icons/units`).
    /// - `FAF_ML_ICON_MODS` — colon-separated icon-set mod directories
    ///   (default: `tmp/ReduxStrategicIconsLarge:tmp/Calibersexp:tmp/SACUIcons`).
    pub fn from_env() -> crate::Result<Self> {
        let root = workspace_root();
        let icon_mods = std::env::var("FAF_ML_ICON_MODS")
            .map(|raw| {
                raw.split(':')
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_else(|_| {
                vec![
                    root.join("tmp/ReduxStrategicIconsLarge"),
                    root.join("tmp/Calibersexp"),
                    root.join("tmp/SACUIcons"),
                ]
            });
        Ok(Self {
            port: crate::env::port_or("FAF_ML_PORT", 3100)?,
            data_dir: crate::env::path_or("FAF_ML_DATA_DIR", root.join("data/faf-ml")),
            assets_dir: crate::env::path_or(
                "FAF_ML_WEB_DIST",
                root.join("target/dx/faf-ml-web/release/web/public"),
            ),
            icons_dir: crate::env::path_or(
                "FAF_ML_ICONS_DIR",
                root.join("tmp/custom-strategic-icons"),
            ),
            portraits_dir: crate::env::path_or(
                "FAF_ML_PORTRAITS_DIR",
                root.join("assets/icons/units"),
            ),
            icon_mods,
        })
    }
}
