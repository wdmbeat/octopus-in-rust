//! Icon-set mod parsing and effective unit → icon resolution.
//!
//! A FAF strategic-icon mod is a directory with:
//!
//! - `mod_info.lua` — `name = "..."`, `version = N` metadata.
//! - `mod_icons.lua` — explicit `UnitIconAssignments` entries
//!   (`{ BlueprintId = "uel0001", IconSet = "icon_commander_uef" }`) plus a
//!   `ScriptedIconAssignments` name-matching fallback (a unit whose
//!   blueprint `StrategicIconName` matches a sprite the mod ships gets the
//!   mod's look under the SAME class name).
//! - `custom-strategic-icons/` — 4-state DDS sprites per icon set
//!   (`{IconSet}_rest.dds` etc.).
//!
//! We parse the Lua with simple line scanning (no Lua interpreter): the two
//! shapes above are stable across the icon-mod convention. This module
//! mirrors what the game does when mods are toggled, so generated training
//! data matches the on-screen icons.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use faf_blueprints::UnitBlueprint;
use faf_ml_core::UnitIconEffective;

/// Sprite state suffixes — longest first so `_selectedover` strips before
/// `_selected`/`_over` (same rule as faf-ml-datagen's `load_sprites`).
const STATE_SUFFIXES: [&str; 4] = ["_selectedover", "_selected", "_over", "_rest"];

/// Normalize a sprite file stem or mod `IconSet` value to a class name:
/// strip state suffix, then a leading `icon_`, then lowercase (mods mix
/// cases between `mod_icons.lua` and file names, e.g. `icon_mavor` vs
/// `icon_Mavor_rest.dds`). Handles both prefix conventions:
/// `icon_chicken_rest` → `chicken`, `SACU_RAS_rest` → `sacu_ras`.
pub fn normalize_class(raw: &str) -> String {
    let base = STATE_SUFFIXES
        .iter()
        .find_map(|s| raw.strip_suffix(s))
        .unwrap_or(raw);
    base.strip_prefix("icon_")
        .unwrap_or(base)
        .to_ascii_lowercase()
}

/// One parsed icon-set source (a mod directory, or the legacy flat icons
/// dir as a metadata-less builtin).
#[derive(Debug, Clone)]
pub struct IconSet {
    /// Directory name, e.g. `"ReduxStrategicIconsLarge"`.
    pub id: String,
    /// Human-readable name from `mod_info.lua` (falls back to `id`).
    pub name: String,
    /// Mod version from `mod_info.lua`, if declared.
    pub version: Option<u32>,
    /// Directory containing the `_rest.dds` sprites.
    pub icons_dir: PathBuf,
    /// Classes this set ships sprites for (normalized names).
    pub classes: HashSet<String>,
    /// Explicit `BlueprintId → class` assignments (blueprint ids uppercased).
    pub assignments: Vec<(String, String)>,
    /// `true` for the legacy flat-icons-dir fallback (no mod metadata).
    pub builtin: bool,
}

impl IconSet {
    /// The class an explicit assignment maps `unit_id` to, if any.
    fn assigned_class(&self, unit_id: &str) -> Option<&str> {
        self.assignments
            .iter()
            .find(|(bpid, _)| bpid == unit_id)
            .map(|(_, class)| class.as_str())
    }
}

/// Parse `mod_info.lua`: line-scan `key = value` pairs (`name`, `version`).
fn parse_mod_info(dir: &Path) -> (Option<String>, Option<u32>) {
    let Ok(raw) = std::fs::read_to_string(dir.join("mod_info.lua")) else {
        return (None, None);
    };
    let mut name = None;
    let mut version = None;
    for line in raw.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("name") {
            name = value
                .trim_start()
                .strip_prefix('=')
                .map(|v| v.trim().trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("version") {
            version = value
                .trim_start()
                .strip_prefix('=')
                .and_then(|v| v.trim().parse::<u32>().ok());
        }
    }
    (name, version)
}

/// Parse `mod_icons.lua`: scan `{ BlueprintId = "...", IconSet = "..." }`
/// entries (whitespace/case tolerant). The `ScriptedIconAssignments`
/// fallback needs no parsing — it is pure name matching against the mod's
/// sprite files, applied at resolution time.
fn parse_mod_icons(dir: &Path) -> Vec<(String, String)> {
    let Ok(raw) = std::fs::read_to_string(dir.join("mod_icons.lua")) else {
        return Vec::new();
    };
    let mut assignments = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with('{') || !line.contains("BlueprintId") {
            continue;
        }
        let Some(bpid) = extract_lua_string(line, "BlueprintId") else {
            continue;
        };
        let Some(icon_set) = extract_lua_string(line, "IconSet") else {
            continue;
        };
        assignments.push((bpid.to_ascii_uppercase(), normalize_class(&icon_set)));
    }
    assignments
}

/// Extract the string value of `Key = "value"` from one Lua line.
fn extract_lua_string(line: &str, key: &str) -> Option<String> {
    let pos = line.find(key)?;
    let rest = line[pos + key.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Collect the classes a `custom-strategic-icons`-style directory ships:
/// every `_rest.dds` (and suffix-less `.dds`), normalized.
fn collect_classes(icons_dir: &Path) -> Result<HashSet<String>> {
    let mut classes = HashSet::new();
    for entry in
        std::fs::read_dir(icons_dir).with_context(|| format!("reading icons dir {icons_dir:?}"))?
    {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".dds") else {
            continue;
        };
        let is_state_variant = STATE_SUFFIXES.iter().any(|s| base.ends_with(s));
        // Only resting-state (or suffix-less) sprites participate.
        if is_state_variant && !base.ends_with("_rest") {
            continue;
        }
        classes.insert(normalize_class(base));
    }
    Ok(classes)
}

/// Parse one icon-set mod directory.
pub fn parse_mod(dir: &Path) -> Result<IconSet> {
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .context("icon mod dir has no name")?;
    let icons_dir = dir.join("custom-strategic-icons");
    let classes = collect_classes(&icons_dir)?;
    let (name, version) = parse_mod_info(dir);
    let assignments = parse_mod_icons(dir);
    Ok(IconSet {
        name: name.unwrap_or_else(|| id.clone()),
        version,
        id,
        icons_dir,
        classes,
        assignments,
        builtin: false,
    })
}

/// Wrap the legacy flat icons dir (`FAF_ML_ICONS_DIR`) as a builtin set.
pub fn builtin_set(icons_dir: &Path) -> Result<IconSet> {
    let classes = collect_classes(icons_dir)?;
    Ok(IconSet {
        id: "vanilla".to_string(),
        name: "Vanilla (flat icons dir)".to_string(),
        version: None,
        icons_dir: icons_dir.to_path_buf(),
        classes,
        assignments: Vec::new(),
        builtin: true,
    })
}

/// Parse every registered mod dir; broken dirs are skipped with a warning.
/// Falls back to the builtin flat icons dir when no mod parses, so datagen
/// keeps working on deployments without the mod checkouts.
pub fn load_icon_sets(mod_dirs: &[PathBuf], fallback_icons_dir: &Path) -> Vec<IconSet> {
    let sets: Vec<IconSet> = mod_dirs
        .iter()
        .filter_map(|dir| match parse_mod(dir) {
            Ok(set) => Some(set),
            Err(err) => {
                tracing::warn!(dir = %dir.display(), "skipping icon mod: {err:#}");
                None
            }
        })
        .collect();
    if !sets.is_empty() {
        return sets;
    }
    match builtin_set(fallback_icons_dir) {
        Ok(set) => vec![set],
        Err(err) => {
            tracing::warn!(dir = %fallback_icons_dir.display(), "no icon sets available: {err:#}");
            Vec::new()
        }
    }
}

/// The enabled sets, in override-priority order (config order preserved).
/// Builtin fallback sets are always enabled; mods must be listed in
/// `enabled_mods` explicitly.
pub fn enabled_sets<'a>(sets: &'a [IconSet], enabled_mods: &[String]) -> Vec<&'a IconSet> {
    sets.iter()
        .filter(|s| s.builtin || enabled_mods.contains(&s.id))
        .collect()
}

/// Compute the effective strategic icon for every unit under a mod
/// selection — the same result as toggling these mods in game:
///
/// 1. Explicit mod assignment (latest enabled mod wins).
/// 2. Blueprint `strategic_icon_name`, when some enabled set ships the
///    class (a mod shipping it reskins the look under the same class).
/// 3. Uncovered (`class: None`).
pub fn compute_effective(units: &[UnitBlueprint], sets: &[&IconSet]) -> Vec<UnitIconEffective> {
    units
        .iter()
        .map(|bp| {
            let unit_id = bp.unit_id().to_ascii_uppercase();
            // Explicit assignments: scan sets in reverse so the last
            // (highest-priority) enabled mod wins.
            let explicit = sets
                .iter()
                .rev()
                .find_map(|set| set.assigned_class(&unit_id).map(|c| (c.to_string(), *set)));
            let resolved = explicit.or_else(|| {
                let class = normalize_class(bp.strategic_icon_name()?);
                let set = sets.iter().rev().find(|set| set.classes.contains(&class));
                set.map(|set| (class, *set))
            });
            let (class, source) = match resolved {
                Some((class, set)) => (Some(class.to_string()), Some(set.id.clone())),
                None => (None, None),
            };
            UnitIconEffective {
                unit_id,
                class,
                source,
            }
        })
        .collect()
}

/// Unit counts per class under a mod selection (blueprint defaults +
/// explicit assignments), used by the class picker and its warnings.
pub fn class_unit_counts(effective: &[UnitIconEffective]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in effective {
        if let Some(class) = &entry.class {
            *counts.entry(class.clone()).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use faf_blueprints::{TechLevel, UnitCostMetrics, UnitEffectEcoMetrics};

    /// Workspace root (tests run with CWD = the crate dir).
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn test_unit(id: &str, strategic_icon_name: Option<&str>) -> UnitBlueprint {
        UnitBlueprint::new(
            id.to_string(),
            id.to_string(),
            UnitCostMetrics::new(0.0, 0.0, 0.0),
            UnitEffectEcoMetrics::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            TechLevel::T1,
            None,
            None,
            strategic_icon_name.map(str::to_string),
        )
    }

    fn test_set(id: &str, classes: &[&str], assignments: &[(&str, &str)]) -> IconSet {
        IconSet {
            id: id.to_string(),
            name: id.to_string(),
            version: None,
            icons_dir: PathBuf::new(),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            assignments: assignments
                .iter()
                .map(|(bpid, class)| (bpid.to_string(), class.to_string()))
                .collect(),
            builtin: false,
        }
    }

    #[test]
    fn parse_real_mods() {
        let root = workspace_root();

        let redux = parse_mod(&root.join("tmp/ReduxStrategicIconsLarge")).unwrap();
        assert_eq!(redux.id, "ReduxStrategicIconsLarge");
        assert_eq!(redux.name, "Redux Strategic Icons Large");
        assert!(
            redux.classes.len() > 180,
            "Redux classes: {}",
            redux.classes.len()
        );
        assert!(redux
            .assignments
            .contains(&("UEL0001".to_string(), "commander_uef".to_string())));

        let calibers = parse_mod(&root.join("tmp/Calibersexp")).unwrap();
        assert_eq!(calibers.assignments.len(), 18);
        assert_eq!(calibers.classes.len(), 18);
        assert!(calibers.classes.contains("chicken"));

        let sacu = parse_mod(&root.join("tmp/SACUIcons")).unwrap();
        assert_eq!(sacu.assignments.len(), 10);
        assert_eq!(sacu.classes.len(), 3);
        assert!(sacu.classes.contains("sacu_ras"));
        assert!(sacu
            .assignments
            .contains(&("URL0301_RAS".to_string(), "sacu_ras".to_string())));
    }

    #[test]
    fn explicit_assignment_beats_blueprint_default() {
        let base = test_set("base", &["commander_generic"], &[]);
        let redux = test_set(
            "redux",
            &["commander_generic", "commander_uef"],
            &[("UEL0001", "commander_uef")],
        );
        let units = vec![test_unit("uel0001", Some("icon_commander_generic"))];
        let sets: Vec<&IconSet> = vec![&base, &redux];
        let effective = compute_effective(&units, &sets);
        assert_eq!(effective[0].class.as_deref(), Some("commander_uef"));
        assert_eq!(effective[0].source.as_deref(), Some("redux"));
    }

    #[test]
    fn blueprint_default_uses_highest_priority_source() {
        let base = test_set("base", &["fighter3_intel"], &[]);
        let redux = test_set("redux", &["fighter3_intel"], &[]);
        let units = vec![test_unit("xsa0302", Some("icon_fighter3_intel"))];
        let sets: Vec<&IconSet> = vec![&base, &redux];
        let effective = compute_effective(&units, &sets);
        assert_eq!(effective[0].class.as_deref(), Some("fighter3_intel"));
        // Same class name, reskinned by the later (higher-priority) set.
        assert_eq!(effective[0].source.as_deref(), Some("redux"));
    }

    #[test]
    fn missing_class_leaves_unit_uncovered() {
        let base = test_set("base", &["bomber1_directfire"], &[]);
        let units = vec![
            test_unit("xsa0302", Some("icon_fighter3_intel")),
            test_unit("dea0202", Some("icon_bomber1_directfire")),
            test_unit("ual0107", None),
        ];
        let sets: Vec<&IconSet> = vec![&base];
        let effective = compute_effective(&units, &sets);
        assert_eq!(effective[0].class, None);
        assert_eq!(effective[1].class.as_deref(), Some("bomber1_directfire"));
        assert_eq!(effective[2].class, None);
        let counts = class_unit_counts(&effective);
        assert_eq!(counts.get("bomber1_directfire"), Some(&1));
        assert_eq!(counts.len(), 1);
    }

    #[test]
    fn enabled_sets_filters_mods_keeps_builtin() {
        let mut vanilla = test_set("vanilla", &[], &[]);
        vanilla.builtin = true;
        let redux = test_set("ReduxStrategicIconsLarge", &[], &[]);
        let sets = vec![vanilla, redux];
        let enabled = enabled_sets(&sets, &["ReduxStrategicIconsLarge".to_string()]);
        assert_eq!(enabled.len(), 2);
        let enabled = enabled_sets(&sets, &[]);
        assert_eq!(enabled.len(), 1);
        assert!(enabled[0].builtin);
    }

    #[test]
    fn normalize_strips_prefix_state_suffix_and_case() {
        assert_eq!(normalize_class("icon_chicken_rest"), "chicken");
        assert_eq!(normalize_class("icon_chicken_selectedover"), "chicken");
        assert_eq!(normalize_class("SACU_RAS_rest"), "sacu_ras");
        assert_eq!(normalize_class("icon_strategic_nuke"), "strategic_nuke");
        assert_eq!(normalize_class("bomber1_directfire"), "bomber1_directfire");
        // Calibersexp mixes cases: assignment `icon_mavor`, file `icon_Mavor_rest`.
        assert_eq!(normalize_class("icon_Mavor_rest"), "mavor");
        assert_eq!(normalize_class("icon_mavor"), "mavor");
    }

    #[test]
    fn extract_lua_string_reads_quoted_value() {
        let line = r#"    { BlueprintId = "uel0001", IconSet = "icon_commander_uef" },"#;
        assert_eq!(
            extract_lua_string(line, "BlueprintId").as_deref(),
            Some("uel0001")
        );
        assert_eq!(
            extract_lua_string(line, "IconSet").as_deref(),
            Some("icon_commander_uef")
        );
    }

    #[test]
    fn parse_assignments_skips_comments_and_blanks() {
        let dir = std::env::temp_dir().join("octopus-test-mod-icons");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod_icons.lua"),
            "UnitIconAssignments = {\n  { BlueprintId = \"xsl0401\", IconSet = \"icon_chicken\" },\n  -- comment\n}\n",
        )
        .unwrap();
        let assignments = parse_mod_icons(&dir);
        assert_eq!(
            assignments,
            vec![("XSL0401".to_string(), "chicken".to_string())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
