//! Unit portrait image route (ported from fafcn-server).

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
};

use crate::{error::Result, state::AppState};

/// `GET /api/portraits/:id` — serve a unit portrait PNG.
pub async fn get_portrait(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let path = state
        .portraits_dir
        .join(format!("{}.png", id.to_ascii_uppercase()));
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| crate::error::Error::NotFound)?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes))
}
