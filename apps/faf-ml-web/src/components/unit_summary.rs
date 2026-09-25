use faf_blueprints::{TechLevel, UnitCostMetrics, UnitEffectEcoMetrics};
use serde::Deserialize;

/// Lightweight unit summary sent by `/api/units` and used throughout the UI.
///
/// Mirrors `apps/faf-ml-server/src/handlers/units.rs::UnitSummary`
/// field-for-field — keep the two in sync.
#[derive(Clone, Deserialize, PartialEq)]
pub struct UnitSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub faction: String,
    pub tech_level: TechLevel,
    pub cost: UnitCostMetrics,
    pub eco_effect: UnitEffectEcoMetrics,
    pub category: Option<String>,
    pub kind: Option<String>,
    pub strategic_icon_name: Option<String>,
}
