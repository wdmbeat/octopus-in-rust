//! Unit listing and blueprint routes.

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json,
};
use faf_blueprints::{TechLevel, UnitBlueprint};
use serde::Serialize;

use crate::{error::Result, state::AppState};

/// Summary sent to the frontend for unit selection.
#[derive(Serialize)]
pub struct UnitSummary {
    id: String,
    name: String,
    description: String,
    faction: String,
    tech_level: TechLevel,
    cost: faf_blueprints::UnitCostMetrics,
    eco_effect: faf_blueprints::UnitEffectEcoMetrics,
    category: Option<String>,
    kind: Option<String>,
    strategic_icon_name: Option<String>,
}

impl From<UnitBlueprint> for UnitSummary {
    fn from(bp: UnitBlueprint) -> Self {
        Self {
            id: bp.unit_id().to_string(),
            name: bp.unit_description().to_string(),
            description: bp.unit_description().to_string(),
            faction: infer_faction(bp.unit_id()).to_string(),
            tech_level: bp.tech_level(),
            cost: bp.unit_cost(),
            eco_effect: bp.unit_eco_effect().clone(),
            category: bp.category().map(|c| c.label().to_string()),
            kind: bp.kind().map(|k| k.label().to_string()),
            strategic_icon_name: bp.strategic_icon_name().map(|s| s.to_string()),
        }
    }
}

/// Best-effort faction inference from the second letter of a blueprint id.
///
/// FAF blueprint ids use the pattern `<category><faction>...`, where the
/// faction letter is `E` (UEF), `A` (Aeon), `R` (Cybran), or `S` (Seraphim).
fn infer_faction(id: &str) -> &str {
    match id.chars().nth(1) {
        Some('E') => "uef",
        Some('A') => "aeon",
        Some('R') => "cybran",
        Some('S') => "seraphim",
        _ => "unknown",
    }
}

/// List all unit summaries.
pub async fn list_units(State(state): State<AppState>) -> impl IntoResponse {
    let units: Vec<UnitSummary> = state
        .blueprints
        .all_units()
        .into_iter()
        .filter(|bp| {
            let faction = infer_faction(bp.unit_id());
            matches!(faction, "uef" | "aeon" | "cybran" | "seraphim")
        })
        .map(UnitSummary::from)
        .collect();
    Json(units)
}

/// Human-readable name of the upstream unit database (for attribution).
const UNITS_SOURCE_NAME: &str = "ETFreeman unit database";
/// Upstream project the unit database is downloaded from by `faf-unit-tools download`.
const UNITS_SOURCE_URL: &str = "https://github.com/FAForever/etfreeman-db";

/// Metadata about the loaded unit database, shown on the Units page.
#[derive(Serialize)]
pub struct UnitsMeta {
    /// FAF patch version of the data, e.g. `"3837"`.
    version: String,
    unit_count: usize,
    source_name: &'static str,
    source_url: &'static str,
}

/// `GET /api/units/meta` — data version and upstream attribution.
///
/// Registered as a static segment, so it wins over `/api/units/:id`.
pub async fn units_meta(State(state): State<AppState>) -> impl IntoResponse {
    Json(UnitsMeta {
        version: state.blueprints.units_version().to_string(),
        unit_count: state.blueprints.all_units().len(),
        source_name: UNITS_SOURCE_NAME,
        source_url: UNITS_SOURCE_URL,
    })
}

/// Get a single unit blueprint by id or search term.
pub async fn get_unit(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let blueprint = state.blueprints.get_one_unit_from_search(&id)?;
    Ok(Json(blueprint))
}
