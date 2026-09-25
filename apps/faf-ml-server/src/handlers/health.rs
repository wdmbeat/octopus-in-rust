//! Health check: `GET /api/health`.

use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::AppState;

/// Response body for `GET /api/health`.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    /// Number of uploaded screenshots in the index.
    pub screenshots: usize,
    /// The built web SPA (index.html) is present.
    pub web_dist_present: bool,
    /// The data store directory layout is present.
    pub data_dir_present: bool,
}

/// Axum handler for `GET /api/health`.
pub async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    let screenshots = crate::handlers::screenshots::read_index(&state)
        .map(|m| m.len())
        .unwrap_or(0);
    Json(HealthResponse {
        status: "ok",
        screenshots,
        web_dist_present: state.assets_dir.join("index.html").is_file(),
        data_dir_present: state.screenshots_dir().is_dir(),
    })
}
