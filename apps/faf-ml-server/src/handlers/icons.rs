//! Icon-configuration routes: `/api/icons/*` — the central place to view
//! and edit the unit ↔ strategic-icon mapping (which icon-set mods are
//! enabled, which classes are excluded from training data).

use std::io::Cursor;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use faf_ml_core::{IconClassInfo, IconConfig, IconSetInfo, UnitIconEffective};
use serde::Deserialize;

use crate::{
    error::{Error, Result},
    icon_sets,
    state::AppState,
};

/// Read `icon-config.json`. An absent file means "default config": every
/// registered mod enabled, nothing excluded (equivalent to running the game
/// with all registered icon mods on).
pub(crate) fn read_icon_config(state: &AppState) -> Result<IconConfig> {
    match std::fs::read_to_string(state.icon_config_path()) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(IconConfig {
            enabled_mods: state
                .icon_sets
                .iter()
                .filter(|s| !s.builtin)
                .map(|s| s.id.clone())
                .collect(),
            excluded_classes: Vec::new(),
        }),
        Err(err) => Err(err.into()),
    }
}

/// Optional `?mods=id1,id2` preview override (absent = saved config).
#[derive(Debug, Deserialize)]
pub struct ModsQuery {
    mods: Option<String>,
}

/// Resolve the enabled mod ids for a request: the `?mods=` preview
/// selection when given, else the saved config.
fn resolve_enabled(state: &AppState, query: &ModsQuery) -> Result<Vec<String>> {
    match &query.mods {
        Some(raw) => Ok(raw
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()),
        None => Ok(read_icon_config(state)?.enabled_mods),
    }
}

/// Four-faction units (same filter as `/api/units`: mod factions like
/// Nomads are not part of the pipeline).
pub(crate) fn pipeline_units(state: &AppState) -> Vec<faf_blueprints::UnitBlueprint> {
    state
        .blueprints
        .all_units()
        .into_iter()
        .filter(|bp| {
            matches!(
                bp.unit_id().chars().nth(1),
                Some('E') | Some('A') | Some('R') | Some('S')
            )
        })
        .collect()
}

/// `GET /api/icons/sets` — every registered icon-set source with metadata
/// and its enabled flag (for the mod-checkbox card).
pub async fn list_icon_sets(State(state): State<AppState>) -> Result<Json<Vec<IconSetInfo>>> {
    let config = read_icon_config(&state)?;
    let infos = state
        .icon_sets
        .iter()
        .map(|set| IconSetInfo {
            id: set.id.clone(),
            name: set.name.clone(),
            version: set.version,
            class_count: set.classes.len(),
            assignment_count: set.assignments.len(),
            enabled: set.builtin || config.enabled_mods.contains(&set.id),
            builtin: set.builtin,
        })
        .collect();
    Ok(Json(infos))
}

/// `GET /api/icons/config` — the saved icon configuration.
pub async fn get_icon_config(State(state): State<AppState>) -> Result<Json<IconConfig>> {
    Ok(Json(read_icon_config(&state)?))
}

/// `PUT /api/icons/config` — persist a new icon configuration. Enabled mod
/// ids must exist among the registered sets.
pub async fn put_icon_config(
    State(state): State<AppState>,
    Json(config): Json<IconConfig>,
) -> Result<Json<IconConfig>> {
    for id in &config.enabled_mods {
        if !state.icon_sets.iter().any(|s| &s.id == id) {
            return Err(Error::BadRequest(format!("unknown icon mod {id:?}")));
        }
    }
    std::fs::write(
        state.icon_config_path(),
        serde_json::to_string_pretty(&config)?,
    )?;
    Ok(Json(config))
}

/// `GET /api/icons/classes` — per-class source, unit coverage, and excluded
/// flag for the include/exclude picker. Only classes that map to at least
/// one unit are listed: orphan marker icons (`strat_attack`, `ferry_point`,
/// ...) can never be trained on, so they are not selectable.
pub async fn list_icon_classes(
    State(state): State<AppState>,
    Query(query): Query<ModsQuery>,
) -> Result<Json<Vec<IconClassInfo>>> {
    let enabled = resolve_enabled(&state, &query)?;
    let sets = icon_sets::enabled_sets(&state.icon_sets, &enabled);
    let effective = icon_sets::compute_effective(&pipeline_units(&state), &sets);
    let counts = icon_sets::class_unit_counts(&effective);
    let config = read_icon_config(&state)?;

    // Union of classes shipped by enabled sets; source = the
    // highest-priority set shipping the class.
    let mut classes: Vec<String> = sets
        .iter()
        .flat_map(|set| set.classes.iter().cloned())
        .collect();
    classes.sort();
    classes.dedup();
    let infos = classes
        .into_iter()
        .filter_map(|class| {
            let unit_count = counts.get(&class).copied().unwrap_or(0);
            if unit_count == 0 {
                return None;
            }
            let source = sets
                .iter()
                .rev()
                .find(|set| set.classes.contains(&class))
                .map(|set| set.id.clone())
                .unwrap_or_default();
            Some(IconClassInfo {
                unit_count,
                excluded: config.excluded_classes.contains(&class),
                class,
                source,
            })
        })
        .collect();
    Ok(Json(infos))
}

/// `GET /api/icons/units` — the effective strategic icon per unit under the
/// saved config (or a `?mods=` preview selection).
pub async fn list_unit_icons(
    State(state): State<AppState>,
    Query(query): Query<ModsQuery>,
) -> Result<Json<Vec<UnitIconEffective>>> {
    let enabled = resolve_enabled(&state, &query)?;
    let sets = icon_sets::enabled_sets(&state.icon_sets, &enabled);
    Ok(Json(icon_sets::compute_effective(
        &pipeline_units(&state),
        &sets,
    )))
}

/// `GET /api/icons/sprites/{class}/image` — the sprite PNG of a class,
/// resolved across enabled sources (an enabled mod's sprite overrides the
/// base set's, like in game). The web UI cannot display the source DDS.
pub async fn get_icon_sprite_image(
    State(state): State<AppState>,
    Path(class): Path<String>,
) -> Result<impl IntoResponse> {
    // The class becomes a file name — refuse anything path-like.
    if !class.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::NotFound);
    }
    let config = read_icon_config(&state)?;
    let sets = icon_sets::enabled_sets(&state.icon_sets, &config.enabled_mods);
    let set = sets
        .iter()
        .rev()
        .find(|set| set.classes.contains(&class))
        .ok_or(Error::NotFound)?;
    let sprite = faf_ml_datagen::load_class_sprite(&set.icons_dir, &class)
        .map_err(|e| Error::Internal(format!("decoding sprite {class}: {e:#}")))?
        .ok_or(Error::NotFound)?;
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(sprite.img)
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| Error::Internal(format!("encoding sprite {class}: {e}")))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
    Ok((headers, png.into_inner()))
}
