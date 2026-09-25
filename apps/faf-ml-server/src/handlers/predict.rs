//! Prediction routes: `GET /api/runs` (checkpoint list) and
//! `POST /api/predict(/annotate)` (run a checkpoint on a store screenshot).
//! Checkpoints are loaded per call (fine for one-off eval; cache later).

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use burn::module::Module;
use faf_ml_core::{DetectionView, PredictRequest, PredictResponse, RunInfo};
use faf_ml_model::{
    anchors::generate_anchors,
    data::image_tensor,
    model::{DetectorConfig, SsdModel},
    predict::{draw_detections, load_rgb, predict, Detection},
};
use image::RgbImage;

use crate::{
    error::{Error, Result},
    state::{valid_dataset_name, AppState},
};

/// Per-class NMS IoU at inference (d2l §14.7-style, same as the old CLI).
const NMS_IOU: f32 = 0.45;

/// `GET /api/runs` — checkpoint runs under `runs/`, newest first.
pub async fn list_runs(State(state): State<AppState>) -> Result<Json<Vec<RunInfo>>> {
    let runs_dir = state.data_dir.join("runs");
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&runs_dir)? {
        let path = entry?.path();
        let config_path = path.join("config.json");
        if !path.is_dir() || !config_path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&config_path)?;
        let config: DetectorConfig = serde_json::from_str(&raw)
            .map_err(|e| Error::Internal(format!("bad {}: {e}", config_path.display())))?;
        runs.push(RunInfo {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            classes: config.classes.len(),
        });
    }
    runs.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(Json(runs))
}

/// `POST /api/predict` — detections as JSON.
pub async fn predict_json(
    State(state): State<AppState>,
    Json(req): Json<PredictRequest>,
) -> Result<Json<PredictResponse>> {
    let (config, detections, _) = run_predict(&state, &req)?;
    let input = config.input_size as f32;
    Ok(Json(PredictResponse {
        run: req.run.clone(),
        image_id: req.image_id,
        detections: detections
            .into_iter()
            .map(|d| DetectionView {
                class: config
                    .classes
                    .get(d.class_id)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string()),
                score: d.score,
                x1: d.bbox.x1 * input,
                y1: d.bbox.y1 * input,
                x2: d.bbox.x2 * input,
                y2: d.bbox.y2 * input,
            })
            .collect(),
    }))
}

/// `POST /api/predict/annotate` — the annotated PNG bytes.
pub async fn predict_annotate(
    State(state): State<AppState>,
    Json(req): Json<PredictRequest>,
) -> Result<impl IntoResponse> {
    let (_, detections, image) = run_predict(&state, &req)?;
    let annotated = draw_detections(&image, &detections);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(annotated)
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| Error::Internal(format!("encoding preview: {e}")))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
    Ok((headers, Bytes::from(png.into_inner())))
}

/// Shared predict plumbing: validate, load checkpoint, detect.
fn run_predict(
    state: &AppState,
    req: &PredictRequest,
) -> Result<(DetectorConfig, Vec<Detection>, RgbImage)> {
    valid_dataset_name(&req.run)?;
    let run_dir = state.data_dir.join("runs").join(&req.run);
    if !run_dir.is_dir() {
        return Err(Error::NotFound);
    }
    let image_path = state.image_path(req.image_id);
    if !image_path.is_file() {
        return Err(Error::NotFound);
    }
    let threshold = req.score_threshold.unwrap_or(0.3);
    let cpu = req.cpu;
    let run_dir = run_dir.clone();
    // GPU/CPU tensor work must not run on the async runtime's core threads.
    tokio::task::block_in_place(|| {
        if cpu {
            run_predict_impl::<faf_ml_model::CpuB>(&run_dir, &image_path, threshold)
        } else {
            run_predict_impl::<faf_ml_model::B>(&run_dir, &image_path, threshold)
        }
    })
}

fn run_predict_impl<B: burn::tensor::backend::Backend>(
    run_dir: &std::path::Path,
    image_path: &std::path::Path,
    threshold: f32,
) -> Result<(DetectorConfig, Vec<Detection>, RgbImage)> {
    let device: burn::tensor::Device<B> = Default::default();
    let config_text = std::fs::read_to_string(run_dir.join("config.json"))?;
    let config: DetectorConfig = serde_json::from_str(&config_text)?;
    let anchors = generate_anchors(&config.anchors, config.input_size);
    let model = SsdModel::<B>::new(&config, &device)
        .load_file(
            run_dir.join("model"),
            &burn::record::CompactRecorder::new(),
            &device,
        )
        .map_err(|e| Error::Internal(format!("loading model: {e}")))?;
    let image = image_tensor::<B>(image_path, config.input_size, &device)?;
    let detections = predict(&model, &anchors, image, threshold, NMS_IOU);
    let rgb = load_rgb(image_path)?;
    Ok((config, detections, rgb))
}
