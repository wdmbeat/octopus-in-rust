use std::{collections::HashMap, path::PathBuf};

use crate::Unit;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Root wrapper for the generated FAF unit index.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FafUnitIndex {
    pub version: String,

    pub shield_default_overspill: f64,
    pub shield_default_recharge_time: f64,

    pub tech_to_vet_multipliers: HashMap<String, f64>,

    /// Per-tech veterancy regen buffs, indexed [tech_index][vet_level].
    pub veterancy_regen_buffs: Vec<Vec<f64>>,

    pub wreckage_tech_mass_mults: HashMap<String, f64>,
    pub wreckage_water_mult: f64,

    #[serde(default)]
    pub units: Vec<Unit>,
}

impl FafUnitIndex {
    pub fn load_default() -> Result<Self> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/faf-units/data/faf_units.json");
        FafUnitIndex::new(path)
    }

    pub fn new(units_file_path: PathBuf) -> Result<Self> {
        let text = std::fs::read_to_string(&units_file_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read units file {}: {e}",
                units_file_path.display()
            )
        })?;

        let index: FafUnitIndex = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse units file {}: {e}",
                units_file_path.display()
            )
        })?;
        Ok(index)
    }

    /// Look up a unit by blueprint id (case-insensitive).
    pub fn find_unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id.eq_ignore_ascii_case(id))
    }

    /// Units matching all of the given categories.
    pub fn units_with_categories<'a>(
        &'a self,
        categories: &'a [&str],
    ) -> impl Iterator<Item = &'a Unit> + 'a {
        self.units
            .iter()
            .filter(move |u| categories.iter().all(|c| u.has_category(c)))
    }

    /// Units whose name, id, description or Chinese translation contains the
    /// query (case-insensitive).
    pub fn search<'a>(&'a self, query: &'a str) -> impl Iterator<Item = &'a Unit> + 'a {
        let query = query.to_lowercase();
        self.units.iter().filter(move |u| {
            u.id.to_lowercase().contains(&query)
                || u.description.to_lowercase().contains(&query)
                || u.name()
                    .map(|n| n.to_lowercase().contains(&query))
                    .unwrap_or(false)
                || u.name_zh()
                    .map(|n| n.to_lowercase().contains(&query))
                    .unwrap_or(false)
                || u.description_zh()
                    .map(|d| d.to_lowercase().contains(&query))
                    .unwrap_or(false)
        })
    }
}
