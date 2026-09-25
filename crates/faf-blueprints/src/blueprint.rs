use crate::{categories::*, eco_metrics::*, unit_meta::*};
use crate::{Error, Result};
use faf_units::{FafUnitIndex, Unit};
use serde::{Deserialize, Serialize};
/// Unified repository of unit knowledge backed by a Bevy ECS blueprint world.
///
/// `BlueprintLibrary` is self-contained: after construction it no longer
/// references the raw `DataIndex`. All build/upgrade rules are explicit recipes
/// rather than derived string-category graphs.
#[derive(Debug)]
pub struct FafBlueprints {
    index: FafUnitIndex,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct UnitBlueprint {
    unit_id: String,
    unit_description: String,
    unit_cost: UnitCostMetrics,
    unit_eco_effect: UnitEffectEcoMetrics,
    tech_level: TechLevel,
    #[serde(default)]
    category: Option<UnitCategory>,
    #[serde(default)]
    kind: Option<UnitKind>,
    #[serde(default)]
    strategic_icon_name: Option<String>,
}

impl UnitBlueprint {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        unit_id: String,
        unit_description: String,
        unit_cost: UnitCostMetrics,
        unit_eco_effect: UnitEffectEcoMetrics,
        tech_level: TechLevel,
        category: Option<UnitCategory>,
        kind: Option<UnitKind>,
        strategic_icon_name: Option<String>,
    ) -> Self {
        Self {
            unit_id,
            unit_description,
            unit_cost,
            unit_eco_effect,
            tech_level,
            category,
            kind,
            strategic_icon_name,
        }
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn unit_description(&self) -> &str {
        &self.unit_description
    }

    pub fn unit_cost(&self) -> UnitCostMetrics {
        self.unit_cost
    }

    pub fn unit_eco_effect(&self) -> &UnitEffectEcoMetrics {
        &self.unit_eco_effect
    }

    pub fn tech_level(&self) -> TechLevel {
        self.tech_level
    }

    pub fn category(&self) -> Option<UnitCategory> {
        self.category
    }

    pub fn kind(&self) -> Option<UnitKind> {
        self.kind
    }

    pub fn strategic_icon_name(&self) -> Option<&str> {
        self.strategic_icon_name.as_deref()
    }
}

impl FafBlueprints {
    pub fn new() -> Result<Self> {
        // The units file path defaults to a compile-time repo location; allow
        // deployments to override it so the binary can run outside the repo.
        let index = match std::env::var("FAFCN_UNITS_FILE") {
            Ok(path) => FafUnitIndex::new(path.into())?,
            Err(_) => FafUnitIndex::load_default()?,
        };
        let blueprint = FafBlueprints { index };
        println!("loaded {} units", blueprint.index.units.len());
        Ok(blueprint)
    }

    pub fn get_units_from_search(&self, search: &str) -> Result<Vec<UnitBlueprint>> {
        let units = self.get_unit_from_search(search)?;
        let mut blueprints: Vec<UnitBlueprint> = Vec::new();

        for each in units {
            let eco_metrics = self.get_eco_cost_from_search(&each)?;
            let eco_effect = self.get_unit_eco_effect(&each)?;
            let tech_level = self.get_unit_tech_level(&each)?;
            let category = Some(classify_category(&each));
            let kind = Some(unit_kind(&each));

            let unit_blueprint = UnitBlueprint {
                unit_id: each.id,
                unit_description: each.description,
                unit_cost: eco_metrics,
                unit_eco_effect: eco_effect,
                tech_level,
                category,
                kind,
                strategic_icon_name: each.strategic_icon_name.clone(),
            };
            blueprints.push(unit_blueprint);
        }
        Ok(blueprints)
    }

    pub fn get_one_unit_from_search(&self, search: &str) -> Result<UnitBlueprint> {
        let units = self.get_units_from_search(search)?;
        if units.is_empty() {
            Err(Error::UnitNotFound(search.to_string()))
        } else if units.len() > 1 {
            Err(Error::Others(format!(
                "There are multiple units find for searching: {search}"
            )))
        } else {
            Ok(units.first().unwrap().clone())
        }
    }

    /// Return a blueprint for every unit in the index.
    ///
    /// Units that are missing required data (economy, tech level) are skipped
    /// rather than failing the whole list.
    pub fn all_units(&self) -> Vec<UnitBlueprint> {
        self.index
            .units
            .iter()
            .filter_map(|unit| self.unit_to_blueprint(unit).ok())
            .collect()
    }

    /// FAF patch version of the loaded unit database (e.g. `"3837"`).
    pub fn units_version(&self) -> &str {
        &self.index.version
    }

    fn unit_to_blueprint(&self, unit: &faf_units::Unit) -> Result<UnitBlueprint> {
        let eco_metrics = self.get_eco_cost_from_search(unit)?;
        let eco_effect = self.get_unit_eco_effect(unit)?;
        let tech_level = self.get_unit_tech_level(unit)?;

        Ok(UnitBlueprint {
            unit_id: unit.id.clone(),
            unit_description: unit.description.clone(),
            unit_cost: eco_metrics,
            unit_eco_effect: eco_effect,
            tech_level,
            category: Some(classify_category(unit)),
            kind: Some(unit_kind(unit)),
            strategic_icon_name: unit.strategic_icon_name.clone(),
        })
    }

    fn get_unit_from_search(&self, search: &str) -> Result<Vec<Unit>> {
        let units: Vec<Unit> = self.index.search(search).cloned().collect();

        Ok(units)
    }

    fn get_eco_cost_from_search(&self, unit: &Unit) -> Result<UnitCostMetrics> {
        let unit_eco = unit
            .economy
            .clone()
            .ok_or(Error::UnitShouldHaveEconomy(Box::new(unit.clone())))?;

        let build_cost_mass = unit_eco.build_cost_mass.unwrap_or(0.0);
        let build_cost_energy = unit_eco.build_cost_energy.unwrap_or(0.0);
        let build_time = unit_eco.build_time.unwrap_or(0.0);
        let eco_metrics = UnitCostMetrics::new(build_cost_mass, build_cost_energy, build_time);
        Ok(eco_metrics)
    }

    fn get_unit_tech_level(&self, unit: &Unit) -> Result<TechLevel> {
        let tech_level_str = unit
            .tech_level()
            .ok_or(Error::UnitMustHasTechLevel(Box::new(unit.clone())))?;
        let tech_level = TechLevel::new(tech_level_str)?;

        Ok(tech_level)
    }

    fn get_unit_eco_effect(&self, unit: &Unit) -> Result<UnitEffectEcoMetrics> {
        let unit_eco = unit
            .economy
            .clone()
            .ok_or(Error::UnitShouldHaveEconomy(Box::new(unit.clone())))?;

        let generate_mass_rate = unit_eco.production_per_second_mass.unwrap_or(0.0);
        let generate_energy_rate = unit_eco.production_per_second_energy.unwrap_or(0.0);
        let maintainance_energy_drain = unit_eco
            .maintenance_consumption_per_second_energy
            .unwrap_or(0.0);
        let increase_mass_storage_capacity = unit_eco.storage_mass.unwrap_or(0.0);
        let increase_energy_storage_capacity = unit_eco.storage_energy.unwrap_or(0.0);
        let build_power = unit_eco.build_rate.unwrap_or(0.0);

        Ok(UnitEffectEcoMetrics::new(
            generate_mass_rate,
            generate_energy_rate,
            maintainance_energy_drain,
            increase_mass_storage_capacity,
            increase_energy_storage_capacity,
            build_power,
        ))
    }
}
