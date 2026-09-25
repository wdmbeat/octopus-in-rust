//! Icon-set configuration wire types: the icon-mod selection
//! (`icon-config.json`), icon-set metadata, per-class info, and the
//! per-unit effective icon — everything the `/api/icons/*` endpoints
//! exchange with the web UI.

use serde::{Deserialize, Serialize};

/// `icon-config.json`: which icon-set mods are enabled and which icon
/// classes are excluded from training data.
///
/// Lives OUTSIDE `icon-map.json` on purpose: the CLI-generated map is a
/// read-only base matching artifact, while this file is the user-edited,
/// server-managed configuration that turns strategic icons into a
/// mod-switchable feature — like enabling/disabling icon mods in game.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IconConfig {
    /// Enabled icon-set mod ids (mod directory names, e.g.
    /// `"ReduxStrategicIconsLarge"`), in override-priority order: a later
    /// mod wins when several assign/ship the same icon.
    #[serde(default)]
    pub enabled_mods: Vec<String>,
    /// Icon classes excluded from synthetic data generation (by sprite
    /// class name, e.g. `commander_generic`).
    #[serde(default)]
    pub excluded_classes: Vec<String>,
}

/// `GET /api/icons/sets` entry: one registered icon-set source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconSetInfo {
    /// Mod directory name, e.g. `"ReduxStrategicIconsLarge"`.
    pub id: String,
    /// Human-readable name from `mod_info.lua`.
    pub name: String,
    /// Mod version from `mod_info.lua`, if declared.
    pub version: Option<u32>,
    /// Number of icon classes the set ships (`*_rest.dds` sprites).
    pub class_count: usize,
    /// Number of explicit `BlueprintId → IconSet` assignments.
    pub assignment_count: usize,
    /// Whether the set is enabled in the current `IconConfig`.
    pub enabled: bool,
    /// `true` for the legacy flat-icons-dir fallback (no mod metadata).
    pub builtin: bool,
}

/// `GET /api/icons/classes` entry: one sprite class with its source and
/// coverage, for the include/exclude picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconClassInfo {
    /// Sprite class name (no `icon_` prefix, no state suffix).
    pub class: String,
    /// Id of the icon set providing the sprite (`"vanilla"` for the
    /// fallback dir; otherwise the highest-priority enabled mod).
    pub source: String,
    /// Units using this class under the current selection (blueprint
    /// default + explicit mod assignments).
    pub unit_count: usize,
    /// Whether the class is excluded from generation.
    pub excluded: bool,
}

/// `GET /api/icons/units` entry: the strategic icon one unit would show
/// in game under a given mod selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitIconEffective {
    pub unit_id: String,
    /// Effective sprite class; `None` = uncovered unit.
    pub class: Option<String>,
    /// Icon set providing the sprite (`None` when `class` is `None`).
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_config_defaults_to_empty() {
        let config: IconConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, IconConfig::default());
        assert!(config.enabled_mods.is_empty());
        assert!(config.excluded_classes.is_empty());
    }

    #[test]
    fn icon_config_round_trips() {
        let config = IconConfig {
            enabled_mods: vec!["ReduxStrategicIconsLarge".to_string()],
            excluded_classes: vec!["commander_generic".to_string()],
        };
        let raw = serde_json::to_string(&config).unwrap();
        let back: IconConfig = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, config);
    }
}
