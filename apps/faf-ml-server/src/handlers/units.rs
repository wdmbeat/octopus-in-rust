//! Unit listing routes for the Units page (ported from fafcn-server).

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json,
};
use faf_blueprints::{TechLevel, UnitBlueprint};
use faf_ml_core::{IconClassMapping, IconMap, SharedUnit, UnitIcons};
use serde::Serialize;

use crate::{
    error::{Error, Result},
    state::AppState,
};

/// Summary sent to the frontend for unit selection.
///
/// Mirrored field-for-field by `apps/faf-ml-web/src/components/unit_summary.rs`
/// — keep the two in sync.
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

/// `GET /api/units/{id}` — one unit summary (case-insensitive exact id).
pub async fn get_unit(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let unit = state
        .blueprints
        .all_units()
        .into_iter()
        .find(|bp| bp.unit_id().eq_ignore_ascii_case(&id))
        .map(UnitSummary::from)
        .ok_or(Error::NotFound)?;
    Ok(Json(unit))
}

/// Read `icon-map.json`; an absent file means "no custom mapping yet".
fn read_icon_map(state: &AppState) -> Result<IconMap> {
    match std::fs::read_to_string(state.icon_map_path()) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(IconMap::default()),
        Err(err) => Err(err.into()),
    }
}

/// `GET /api/units/{id}/icons` — the unit's blueprint default icon plus
/// every custom-set icon class mapped to it (with the other units sharing
/// each class, so icon ambiguity is visible in the UI). Sharing units are
/// restricted to the four FAF factions (mod factions like Nomads are
/// dropped) and carry their display names.
pub async fn get_unit_icons(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<UnitIcons>> {
    let all_units = state.blueprints.all_units();
    let bp = all_units
        .iter()
        .find(|bp| bp.unit_id().eq_ignore_ascii_case(&id))
        .ok_or(Error::NotFound)?;
    let unit_id = bp.unit_id().to_ascii_uppercase();
    let blueprint_icon = bp.strategic_icon_name().map(|s| s.to_string());

    // id → display name (nickname if any, else ordinary name), four FAF
    // factions only (same filter as the unit listing: mod factions like
    // Nomads are not part of the pipeline).
    let names: std::collections::HashMap<String, String> = all_units
        .iter()
        .filter(|bp| {
            matches!(
                infer_faction(bp.unit_id()),
                "uef" | "aeon" | "cybran" | "seraphim"
            )
        })
        .map(|bp| {
            let id = bp.unit_id().to_ascii_uppercase();
            let name = state
                .unit_display_names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| bp.unit_description().to_string());
            (id, name)
        })
        .collect();

    let map = read_icon_map(&state)?;
    let mut mapped_icons: Vec<IconClassMapping> = map
        .mapping
        .iter()
        .filter(|(_, units)| units.iter().any(|u| u.eq_ignore_ascii_case(&unit_id)))
        .map(|(class, units)| IconClassMapping {
            class: class.clone(),
            units: units
                .iter()
                .filter_map(|u| {
                    names.get(&u.to_ascii_uppercase()).map(|name| SharedUnit {
                        id: u.to_ascii_uppercase(),
                        name: name.clone(),
                    })
                })
                .collect(),
        })
        .collect();
    mapped_icons.sort_by(|a, b| a.class.cmp(&b.class));

    Ok(Json(UnitIcons {
        unit_id,
        blueprint_icon,
        mapped_icons,
    }))
}

/// `GET /api/units` — all unit summaries (four playable factions only).
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
pub async fn units_meta(State(state): State<AppState>) -> impl IntoResponse {
    Json(UnitsMeta {
        version: state.blueprints.units_version().to_string(),
        unit_count: state.blueprints.all_units().len(),
        source_name: UNITS_SOURCE_NAME,
        source_url: UNITS_SOURCE_URL,
    })
}
