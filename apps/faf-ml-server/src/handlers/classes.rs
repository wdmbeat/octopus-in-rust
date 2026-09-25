//! Class list route (`GET /api/classes`).

use axum::{extract::State, Json};

use crate::{error::Result, state::AppState};

/// Read `classes.txt` (one class per line; line number = class id).
pub fn read_classes(state: &AppState) -> Result<Vec<String>> {
    match std::fs::read_to_string(state.classes_path()) {
        Ok(raw) => Ok(raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

/// `GET /api/classes` — the full class list.
pub async fn get_classes(State(state): State<AppState>) -> Result<Json<Vec<String>>> {
    Ok(Json(read_classes(&state)?))
}
