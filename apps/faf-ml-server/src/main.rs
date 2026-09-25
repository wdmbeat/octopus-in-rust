//! Backend server for the faf-ml data platform (collect → review → generate
//! → snapshot).
//!
//! Serves the Dioxus web build as static files and exposes:
//!
//! - `POST /api/screenshots` — multipart PNG upload.
//! - `GET /api/screenshots` — list screenshot metadata.
//! - `GET /api/screenshots/:id/image` — serve the PNG.
//! - `GET/PUT /api/screenshots/:id/labels` — read/replace bounding boxes.
//! - `DELETE /api/screenshots/:id` — remove image + labels.
//! - `DELETE /api/screenshots?kind=...` — bulk-remove every screenshot of
//!   one kind (e.g. clearing all `synthetic` samples, which also drops
//!   finished datagen jobs from the registry).
//! - `GET /api/classes` — class list.
//! - `POST /api/datagen` — start a synthetic-data generation job (background
//!   task streaming samples into the store as `synthetic` screenshots; the
//!   sprite pool comes from the icon configuration — see `/api/icons/*`).
//! - `GET /api/datagen/sprites` — list selectable sprite class names.
//! - `GET /api/datagen/sprites/:class/image` — serve one sprite as PNG.
//! - `GET /api/datagen/jobs[/{id}]` — poll job progress.
//! - `DELETE /api/datagen/jobs/:id` — remove a finished job and the sample
//!   set it generated.
//! - `GET/POST /api/datasets` — list / create immutable dataset snapshots.
//! - `GET /api/units` + `GET /api/units/meta` + `GET /api/units/:id` — unit
//!   database for the Units pages (shared ETFreeman database via
//!   `faf-blueprints`).
//! - `GET /api/portraits/:id` — unit portrait PNGs for the Units page.
//! - `GET /ws/training` — WebSocket training: `Start {config, speed}` starts
//!   a REAL burn training thread (managed by the `faf-ml-model` training
//!   manager actor; survives viewer disconnects); `Attach` replays + streams
//!   the current/last run; `Command` frames pause/resume/stop/reset/change
//!   speed.
//! - `GET /api/training/status` — the run registry as JSON.
//! - `GET /api/runs` — checkpoint list; `POST /api/predict(/annotate)` — run
//!   a checkpoint on a store screenshot (JSON detections / annotated PNG).

mod config;
mod env;
mod error;
mod handlers;
mod icon_sets;
mod routes;
mod state;

use anyhow::Context;
use axum::http::{header, Method};
use error::Result;
use tower_http::cors::{Any, CorsLayer};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::{config::ServerConfig, state::AppState};

/// Initialize tracing so logs go to both stdout and `data/logs/faf-ml-server.log`.
///
/// The returned guard must be kept alive for the lifetime of the process so
/// the non-blocking file writer flushes before shutdown.
fn init_tracing() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = config::workspace_root().join("data/logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create log directory {}", log_dir.display()))?;

    let file_appender = tracing_appender::rolling::never(&log_dir, "faf-ml-server.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    env::load();
    let _tracing_guard = init_tracing()?;

    let server_config = ServerConfig::from_env()?;
    let state = AppState::new(
        server_config.data_dir.clone(),
        server_config.assets_dir.clone(),
        server_config.icons_dir.clone(),
        server_config.portraits_dir.clone(),
        server_config.icon_mods.clone(),
    )?;

    tracing::info!(data_dir = %state.data_dir.display(), "data store ready");
    tracing::info!(
        "serving static assets from {}",
        server_config.assets_dir.display()
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::CONTENT_TYPE]);

    let app = routes::router()
        .fallback_service(
            ServeDir::new(state.assets_dir.as_ref())
                .fallback(ServeFile::new(state.assets_dir.join("index.html"))),
        )
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", server_config.port)).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}
