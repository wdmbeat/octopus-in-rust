//! Label read/replace routes (`GET/PUT /api/screenshots/:id/labels`).

use axum::{
    extract::{Path, State},
    Json,
};
use faf_ml_core::LabeledBox;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    state::AppState,
};

/// Read a screenshot's label file; an absent file means "no labels yet".
pub fn read_labels(state: &AppState, id: Uuid) -> Result<Vec<LabeledBox>> {
    match std::fs::read_to_string(state.labels_path(id)) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

/// `GET /api/screenshots/:id/labels` — the box list (empty when unlabeled).
pub async fn get_labels(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<LabeledBox>>> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    if !state.image_path(id).is_file() {
        return Err(Error::NotFound);
    }
    Ok(Json(read_labels(&state, id)?))
}

/// `PUT /api/screenshots/:id/labels` — replace the whole box list.
pub async fn put_labels(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(labels): Json<Vec<LabeledBox>>,
) -> Result<Json<Vec<LabeledBox>>> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    if !state.image_path(id).is_file() {
        return Err(Error::NotFound);
    }
    std::fs::write(
        state.labels_path(id),
        serde_json::to_string_pretty(&labels)?,
    )?;
    Ok(Json(labels))
}
