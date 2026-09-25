//! Screenshot upload, listing, image serving, and deletion.

use axum::{
    body::Bytes,
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use faf_ml_core::{ScreenshotKind, ScreenshotMeta};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    state::AppState,
};

/// Extract `(width, height)` from a PNG byte stream's IHDR chunk.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIG || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Read the screenshot metadata index (`screenshots/index.json`).
pub fn read_index(state: &AppState) -> Result<Vec<ScreenshotMeta>> {
    match std::fs::read_to_string(state.index_path()) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

/// Rewrite the screenshot metadata index.
pub fn write_index(state: &AppState, metas: &[ScreenshotMeta]) -> Result<()> {
    let raw = serde_json::to_string_pretty(metas)?;
    std::fs::write(state.index_path(), raw)?;
    Ok(())
}

/// Store one PNG as a new screenshot (image file + index entry).
/// `job_id` links a synthetic sample to the datagen job that produced it.
pub fn store_screenshot(
    state: &AppState,
    filename: &str,
    bytes: &[u8],
    kind: ScreenshotKind,
    job_id: Option<Uuid>,
) -> Result<ScreenshotMeta> {
    let (width, height) = png_dimensions(bytes)
        .ok_or_else(|| Error::BadRequest(format!("{filename:?} is not a valid PNG")))?;
    let meta = ScreenshotMeta {
        id: Uuid::new_v4(),
        filename: filename.to_string(),
        width,
        height,
        uploaded_at: Utc::now(),
        kind,
        job_id,
    };
    std::fs::write(state.image_path(meta.id), bytes)?;
    let mut metas = read_index(state)?;
    metas.push(meta.clone());
    write_index(state, &metas)?;
    Ok(meta)
}

/// `POST /api/screenshots?kind=battle|background` — multipart upload of one
/// or more PNG files. `kind` defaults to `unclassified` (shown as "needs
/// triage" in the web UI until a human marks the shot).
///
/// Every form field carrying a file is stored; returns the metadata of all
/// newly created screenshots.
#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    kind: ScreenshotKind,
}

pub async fn upload_screenshots(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<Vec<ScreenshotMeta>>> {
    let mut uploaded = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| Error::BadRequest(format!("invalid multipart body: {e}")))?
    {
        let Some(filename) = field.file_name().map(|f| f.to_string()) else {
            continue;
        };
        let bytes = field
            .bytes()
            .await
            .map_err(|e| Error::BadRequest(format!("failed to read {filename:?}: {e}")))?;
        uploaded.push(store_screenshot(
            &state, &filename, &bytes, query.kind, None,
        )?);
    }
    if uploaded.is_empty() {
        return Err(Error::BadRequest("no files in multipart body".to_string()));
    }
    Ok(Json(uploaded))
}

/// `GET /api/screenshots` — list all screenshot metadata.
pub async fn list_screenshots(State(state): State<AppState>) -> Result<Json<Vec<ScreenshotMeta>>> {
    Ok(Json(read_index(&state)?))
}

/// `GET /api/screenshots/:id/image` — serve the PNG bytes.
pub async fn get_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    let bytes: Bytes = std::fs::read(state.image_path(id))?.into();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
    Ok((headers, bytes))
}

/// `PATCH /api/screenshots/:id` — update metadata (the post-upload triage:
/// marking a shot as `battle` vs `background`).
#[derive(Debug, Deserialize)]
pub struct UpdateScreenshot {
    kind: ScreenshotKind,
}

pub async fn update_screenshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(update): Json<UpdateScreenshot>,
) -> Result<Json<ScreenshotMeta>> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    let mut metas = read_index(&state)?;
    let meta = metas
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or(Error::NotFound)?;
    meta.kind = update.kind;
    let updated = meta.clone();
    write_index(&state, &metas)?;
    Ok(Json(updated))
}

/// `DELETE /api/screenshots/:id` — remove the image, its labels, and its
/// index entry.
pub async fn delete_screenshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    let mut metas = read_index(&state)?;
    let before = metas.len();
    metas.retain(|m| m.id != id);
    if metas.len() == before {
        return Err(Error::NotFound);
    }
    write_index(&state, &metas)?;
    let _ = std::fs::remove_file(state.image_path(id));
    let _ = std::fs::remove_file(state.labels_path(id));
    Ok(StatusCode::NO_CONTENT)
}

/// Remove every screenshot matching `pred` (image + labels + index entry);
/// shared by bulk deletion and datagen-job cascade deletion. Returns the
/// number of screenshots removed.
pub fn delete_matching(state: &AppState, pred: impl Fn(&ScreenshotMeta) -> bool) -> Result<usize> {
    let mut metas = read_index(state)?;
    let (victims, kept): (Vec<ScreenshotMeta>, Vec<ScreenshotMeta>) =
        metas.drain(..).partition(|m| pred(m));
    if victims.is_empty() {
        return Ok(0);
    }
    write_index(state, &kept)?;
    for meta in &victims {
        let _ = std::fs::remove_file(state.image_path(meta.id));
        let _ = std::fs::remove_file(state.labels_path(meta.id));
    }
    Ok(victims.len())
}

/// `DELETE /api/screenshots?kind=synthetic` — bulk-remove every screenshot
/// of one kind (the kind is mandatory so this can never nuke everything by
/// accident). Returns the number removed.
///
/// Clearing `synthetic` also drops finished datagen jobs from the registry:
/// their sample sets no longer exist, so a "done — N samples" row would be
/// a lie. Running jobs are left alone (they are still producing samples).
#[derive(Debug, Deserialize)]
pub struct BulkDeleteQuery {
    kind: ScreenshotKind,
}

pub async fn bulk_delete_screenshots(
    State(state): State<AppState>,
    Query(query): Query<BulkDeleteQuery>,
) -> Result<String> {
    let kind = query.kind;
    let removed = delete_matching(&state, |m| m.kind == kind)?;
    if kind == ScreenshotKind::Synthetic {
        state
            .jobs
            .lock()
            .expect("job registry mutex poisoned")
            .retain(|_, job| matches!(job.status, faf_ml_core::DatagenStatus::Running { .. }));
    }
    Ok(format!("removed {removed} {kind} screenshot(s)"))
}
