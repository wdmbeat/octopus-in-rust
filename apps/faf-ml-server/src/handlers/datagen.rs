//! Datagen job routes: `POST /api/datagen` + `GET /api/datagen/jobs[/{id}]`.
//!
//! Generation runs in-process via `tokio::task::spawn_blocking`: each sample
//! is streamed straight into the platform store (a `synthetic`-kind
//! screenshot plus its labels JSON), so the Gallery fills up while the job
//! runs. Progress lives in the in-memory job registry (`AppState::jobs`) and
//! is polled by the web UI — no WebSocket until training arrives in phase 2.
//!
//! Only `background`-kind screenshots may become compositing canvases:
//! battle screenshots contain real units that would end up as unlabeled
//! ghosts in the synthetic data.

use std::io::Cursor;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use faf_ml_core::{
    DatagenConfig, DatagenJob, DatagenStatus, LabeledBox, ScreenshotKind, ScreenshotMeta,
};
use image::RgbaImage;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    state::AppState,
};

/// Reject configs that cannot produce a sample.
fn validate(config: &DatagenConfig) -> Result<()> {
    if config.count == 0 {
        return Err(Error::BadRequest("count must be >= 1".to_string()));
    }
    if config.size == 0 {
        return Err(Error::BadRequest("size must be >= 1".to_string()));
    }
    if config.max_units == 0 {
        return Err(Error::BadRequest("max_units must be >= 1".to_string()));
    }
    if !(config.scale_min > 0.0 && config.scale_min < config.scale_max) {
        return Err(Error::BadRequest(
            "scale_min must be > 0 and < scale_max".to_string(),
        ));
    }
    Ok(())
}

/// Merge class names into the store's `classes.txt`, returning the count of
/// newly appended names.
fn merge_classes(state: &AppState, source: &[String]) -> Result<usize> {
    let mut classes = super::classes::read_classes(state)?;
    let before = classes.len();
    for name in source {
        if !classes.contains(name) {
            classes.push(name.clone());
        }
    }
    let added = classes.len() - before;
    if added > 0 {
        let mut raw = classes.join("\n");
        raw.push('\n');
        std::fs::write(state.classes_path(), raw)?;
    }
    Ok(added)
}

/// Store one generated sample: PNG-encode, register as a `synthetic`
/// screenshot (tagged with the producing job), write its labels JSON.
fn store_sample(
    state: &AppState,
    job_id: Uuid,
    index: usize,
    img: RgbaImage,
    boxes: Vec<faf_ml_datagen::GenBox>,
) -> anyhow::Result<ScreenshotMeta> {
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut png, image::ImageFormat::Png)?;
    let meta = super::screenshots::store_screenshot(
        state,
        &format!("datagen-{index:06}.png"),
        png.get_ref(),
        ScreenshotKind::Synthetic,
        Some(job_id),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let labels: Vec<LabeledBox> = boxes
        .into_iter()
        .map(|b| LabeledBox {
            class: b.class_name,
            x: b.x as f32,
            y: b.y as f32,
            w: b.w as f32,
            h: b.h as f32,
        })
        .collect();
    if !labels.is_empty() {
        std::fs::write(
            state.labels_path(meta.id),
            serde_json::to_string_pretty(&labels)?,
        )?;
    }
    Ok(meta)
}

/// The blocking half of a datagen job: decode the background pool, generate,
/// stream samples into the store, report progress into the job registry.
fn run_datagen_job(
    state: &AppState,
    config: &DatagenConfig,
    sprites: Vec<faf_ml_datagen::Sprite>,
    background_metas: &[ScreenshotMeta],
    job_id: Uuid,
) -> anyhow::Result<usize> {
    let mut backgrounds = Vec::new();
    for meta in background_metas {
        let img = image::open(state.image_path(meta.id))?.to_rgba8();
        if img.width() >= config.size && img.height() >= config.size {
            backgrounds.push(img);
        }
    }
    anyhow::ensure!(
        !backgrounds.is_empty(),
        "no background screenshot is at least {}×{}",
        config.size,
        config.size
    );

    let mut stored = 0usize;
    // The generation callback is infallible, so the first store error is
    // recorded here and returned after generation stops early below.
    let mut first_error: Option<anyhow::Error> = None;
    faf_ml_datagen::generate(config, &sprites, &backgrounds, &mut |img, boxes| {
        if first_error.is_some() {
            return;
        }
        match store_sample(state, job_id, stored, img, boxes) {
            Ok(_) => {
                stored += 1;
                if let Some(job) = state
                    .jobs
                    .lock()
                    .expect("job registry mutex poisoned")
                    .get_mut(&job_id)
                {
                    job.status = DatagenStatus::Running {
                        done: stored,
                        total: config.count,
                    };
                }
            }
            Err(err) => first_error = Some(err),
        }
    });
    if let Some(err) = first_error {
        return Err(err);
    }
    Ok(stored)
}

/// `POST /api/datagen` (body = `DatagenConfig`) — start a generation job.
///
/// 400 when the background pool is empty (triage screenshots in the Gallery
/// first). Sprite loading and the classes.txt merge happen synchronously so
/// a missing icon set fails the request instead of the job.
///
/// The sprite pool comes from the icon configuration (`icon-config.json`,
/// editable on the Icons page): sprites of the enabled icon-set mods,
/// minus the configured excluded classes.
pub async fn start_datagen(
    State(state): State<AppState>,
    Json(config): Json<DatagenConfig>,
) -> Result<Json<DatagenJob>> {
    validate(&config)?;

    let background_metas: Vec<ScreenshotMeta> = super::screenshots::read_index(&state)?
        .into_iter()
        .filter(|m| m.kind == ScreenshotKind::Background)
        .collect();
    if background_metas.is_empty() {
        return Err(Error::BadRequest(
            "no background screenshots: upload empty-terrain shots and mark them \
             \"background\" in the Gallery first"
                .to_string(),
        ));
    }

    let icon_config = super::icons::read_icon_config(&state)?;
    let sets = crate::icon_sets::enabled_sets(&state.icon_sets, &icon_config.enabled_mods);
    let dirs: Vec<&std::path::Path> = sets.iter().map(|set| set.icons_dir.as_path()).collect();
    let mut sprites = faf_ml_datagen::load_sprites_multi(&dirs)
        .map_err(|e| Error::Internal(format!("loading sprites: {e:#}")))?;
    if sprites.is_empty() {
        return Err(Error::Internal(
            "no sprites: no icon set is enabled (configure one on the Icons page)".to_string(),
        ));
    }
    // Only classes that map to at least one unit are trainable — orphan
    // marker icons (`strat_attack`, `ferry_point`, ...) never appear on a
    // unit, so they are dropped before the classes.txt merge as well.
    let effective =
        crate::icon_sets::compute_effective(&super::icons::pipeline_units(&state), &sets);
    let covered: std::collections::HashSet<String> =
        crate::icon_sets::class_unit_counts(&effective)
            .into_keys()
            .collect();
    sprites.retain(|s| covered.contains(&s.class_name));
    if sprites.is_empty() {
        return Err(Error::Internal(
            "no sprites map to any unit under the enabled icon sets".to_string(),
        ));
    }
    // classes.txt is the global training vocabulary: merge ALL enabled
    // unit-mapped sprite class names (not just this run's selection) so
    // class ids stay stable.
    merge_classes(&state, &faf_ml_datagen::class_names(&sprites))?;
    if !icon_config.excluded_classes.is_empty() {
        sprites.retain(|s| !icon_config.excluded_classes.contains(&s.class_name));
        if sprites.is_empty() {
            return Err(Error::BadRequest(
                "the icon configuration excludes every sprite class".to_string(),
            ));
        }
    }

    let job = DatagenJob {
        id: Uuid::new_v4(),
        config: config.clone(),
        started_at: Utc::now(),
        status: DatagenStatus::Running {
            done: 0,
            total: config.count,
        },
    };
    state
        .jobs
        .lock()
        .expect("job registry mutex poisoned")
        .insert(job.id, job.clone());

    let task_state = state.clone();
    let job_id = job.id;
    tokio::task::spawn_blocking(move || {
        let result = run_datagen_job(&task_state, &config, sprites, &background_metas, job_id);
        if let Some(job) = task_state
            .jobs
            .lock()
            .expect("job registry mutex poisoned")
            .get_mut(&job_id)
        {
            job.status = match result {
                Ok(generated) => DatagenStatus::Done { generated },
                Err(err) => DatagenStatus::Failed {
                    error: format!("{err:#}"),
                },
            };
        }
    });

    Ok(Json(job))
}

/// `GET /api/datagen/jobs` — all jobs, newest first.
pub async fn list_datagen_jobs(State(state): State<AppState>) -> Result<Json<Vec<DatagenJob>>> {
    let mut jobs: Vec<DatagenJob> = state
        .jobs
        .lock()
        .expect("job registry mutex poisoned")
        .values()
        .cloned()
        .collect();
    jobs.sort_by_key(|job| std::cmp::Reverse(job.started_at));
    Ok(Json(jobs))
}

/// `GET /api/datagen/jobs/{id}` — one job (for polling).
pub async fn get_datagen_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DatagenJob>> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    let job = state
        .jobs
        .lock()
        .expect("job registry mutex poisoned")
        .get(&id)
        .cloned()
        .ok_or(Error::NotFound)?;
    Ok(Json(job))
}

/// `GET /api/datagen/sprites` — sorted class names of every sprite in the
/// legacy flat icons dir. Superseded by `GET /api/icons/classes` (the Icons
/// page picker); kept for API compatibility.
pub async fn list_sprite_classes(State(state): State<AppState>) -> Result<Json<Vec<String>>> {
    let sprites = faf_ml_datagen::load_sprites(&state.icons_dir)
        .map_err(|e| Error::Internal(format!("loading sprites: {e:#}")))?;
    Ok(Json(faf_ml_datagen::class_names(&sprites)))
}

/// `GET /api/datagen/sprites/{class}/image` — the sprite PNG (the web UI's
/// icon picker cannot display the source DDS).
pub async fn get_sprite_image(
    State(state): State<AppState>,
    Path(class): Path<String>,
) -> Result<impl IntoResponse> {
    // The class becomes a file name — refuse anything path-like.
    if !class
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(Error::NotFound);
    }
    let sprite = faf_ml_datagen::load_class_sprite(&state.icons_dir, &class)
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

/// `DELETE /api/datagen/jobs/{id}` — delete a finished job AND the sample
/// set it generated (image files, labels JSONs, index entries). Samples
/// generated before job tracking exist have no `job_id`; remove those via
/// `DELETE /api/screenshots?kind=synthetic` instead.
pub async fn delete_datagen_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<String> {
    let id: Uuid = id.parse().map_err(|_| Error::NotFound)?;
    let job = state
        .jobs
        .lock()
        .expect("job registry mutex poisoned")
        .get(&id)
        .cloned()
        .ok_or(Error::NotFound)?;
    if matches!(job.status, DatagenStatus::Running { .. }) {
        return Err(Error::BadRequest(
            "job is still running — wait for it to finish before deleting".to_string(),
        ));
    }
    let removed = super::screenshots::delete_matching(&state, |m| m.job_id == Some(id))?;
    state
        .jobs
        .lock()
        .expect("job registry mutex poisoned")
        .remove(&id);
    Ok(format!("removed job {id} and {removed} sample(s)"))
}
