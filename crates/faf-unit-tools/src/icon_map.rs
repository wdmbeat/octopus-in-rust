//! `icon-map` subcommand — cross-check strategic-icon sprites against the
//! unit database.
//!
//! Why this exists: the faf-ml detector's CLASSES are the strategic icon
//! names (e.g. `bomber1_directfire`), and the analysis view looks units up
//! via `Unit::strategic_icon_name`. Both sides must agree, so this tool
//! reports:
//!
//!   1. MATCHED   — sprite classes that map to ≥1 unit (the real classes)
//!   2. ORPHANS   — sprite classes no unit uses (markers like
//!      `strategic_nuke`, modded/extra icons) → exclude from training classes
//!   3. UNCOVERED — icon names in the unit DB with no sprite in the set →
//!      units the detector can never see; the icon set is missing sprites for
//!      them
//!
//! With `--out`, also writes the mapping as a JSON artifact
//! (class → unit ids) for the faf-ml platform's analysis view.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use faf_units::FafUnitIndex;
use serde::Serialize;

const DEFAULT_ICONS_DIR: &str = "tmp/custom-strategic-icons";
const DEFAULT_UNITS_JSON: &str = "plugins/faf-units/data/faf_units.json";

#[derive(Debug, Args)]
pub struct IconMapArgs {
    /// Directory with the strategic-icon .dds files.
    #[arg(long, default_value = DEFAULT_ICONS_DIR)]
    icons: PathBuf,

    /// Path to the unit index JSON (the downloader's output).
    #[arg(long, default_value = DEFAULT_UNITS_JSON)]
    units: PathBuf,

    /// Optional: write the icon→units mapping artifact as JSON.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Sprite state suffixes — longest first so `_selectedover` strips before
/// `_selected`/`_over` (same rule as faf-ml-datagen's `load_sprites`).
const STATE_SUFFIXES: [&str; 4] = ["_selectedover", "_selected", "_over", "_rest"];

/// Icon class name from a sprite filename:
/// `icon_bomber1_directfire_rest.dds` → `bomber1_directfire`,
/// `icon_strategic_nuke.dds` → `strategic_nuke`.
fn class_name(file_name: &str) -> Option<&str> {
    let base = file_name.strip_prefix("icon_")?.strip_suffix(".dds")?;
    Some(
        STATE_SUFFIXES
            .iter()
            .find_map(|s| base.strip_suffix(s))
            .unwrap_or(base),
    )
}

/// Only the resting state (or suffix-less icons) participate — over/selected
/// variants are the same class with UI markers.
fn is_rest_variant(file_name: &str) -> bool {
    file_name.ends_with("_rest.dds")
        || !STATE_SUFFIXES
            .iter()
            .any(|s| file_name.ends_with(&format!("{s}.dds")))
}

#[derive(Serialize)]
struct IconMapArtifact {
    /// class name → unit ids using that strategic icon.
    mapping: BTreeMap<String, Vec<String>>,
    /// sprite classes no unit in the DB references.
    orphan_icons: Vec<String>,
    /// icon names used by units but missing from the sprite set.
    uncovered_icon_names: Vec<String>,
}

pub fn run(args: IconMapArgs) -> Result<()> {
    // ── sprite classes ──────────────────────────────────────────────────────
    let mut classes: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&args.icons)
        .with_context(|| format!("reading icons dir {:?}", args.icons))?
    {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if is_rest_variant(&name) {
            if let Some(class) = class_name(&name) {
                classes.push(class.to_string());
            }
        }
    }
    classes.sort();
    classes.dedup();

    // ── unit DB: icon name → units ──────────────────────────────────────────
    let text = std::fs::read_to_string(&args.units)
        .with_context(|| format!("reading units file {:?}", args.units))?;
    let index: FafUnitIndex = serde_json::from_str(&text).context("failed to parse units JSON")?;

    let mut by_icon: BTreeMap<String, Vec<&faf_units::Unit>> = BTreeMap::new();
    for unit in &index.units {
        if let Some(icon) = &unit.strategic_icon_name {
            by_icon.entry(icon.clone()).or_default().push(unit);
        }
    }

    // ── the three lists ─────────────────────────────────────────────────────
    let mut mapping: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut orphans: Vec<String> = Vec::new();
    for class in &classes {
        let db_name = format!("icon_{class}");
        match by_icon.get(&db_name) {
            Some(units) => {
                mapping.insert(class.clone(), units.iter().map(|u| u.id.clone()).collect());
            }
            None => orphans.push(class.clone()),
        }
    }
    let uncovered: Vec<String> = by_icon
        .keys()
        .filter(|icon| !classes.iter().any(|c| format!("icon_{c}") == **icon))
        .cloned()
        .collect();

    // ── report ──────────────────────────────────────────────────────────────
    println!("=== icon ↔ unit database mapping ===");
    println!("sprite classes:   {}", classes.len());
    println!("unit icon names:  {}", by_icon.len());
    println!();
    println!(
        "MATCHED:   {} classes map to {} units",
        mapping.len(),
        mapping.values().map(Vec::len).sum::<usize>()
    );
    println!(
        "ORPHANS:   {} sprite classes used by no unit",
        orphans.len()
    );
    for o in &orphans {
        println!("  - {o}");
    }
    println!(
        "UNCOVERED: {} unit icon names have no sprite in the set",
        uncovered.len()
    );
    for u in &uncovered {
        let example = by_icon[u].first().map(|u| u.id.as_str()).unwrap_or("?");
        println!("  - {u} (e.g. unit {example})");
    }

    if let Some(out) = &args.out {
        let artifact = IconMapArtifact {
            mapping,
            orphan_icons: orphans,
            uncovered_icon_names: uncovered,
        };
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, serde_json::to_string_pretty(&artifact)?)
            .with_context(|| format!("writing {out:?}"))?;
        println!("\nmapping artifact written to {}", out.display());
    }
    Ok(())
}
