//! Application route table.
//!
//! This module is the single place where every endpoint is mounted. Feature
//! handlers live in `crate::handlers`; this file only wires them to paths.

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, post},
    Router,
};

use crate::{handlers, state::AppState};

/// Build the Axum router for `faf-ml-server`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/health", get(handlers::health::health_handler))
        .route(
            "/api/screenshots",
            post(handlers::screenshots::upload_screenshots)
                .get(handlers::screenshots::list_screenshots)
                .delete(handlers::screenshots::bulk_delete_screenshots)
                // Screenshots are a few MB each; lift the 2 MB default.
                .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/api/screenshots/{id}/image",
            get(handlers::screenshots::get_image),
        )
        .route(
            "/api/screenshots/{id}/labels",
            get(handlers::labels::get_labels).put(handlers::labels::put_labels),
        )
        .route(
            "/api/screenshots/{id}",
            delete(handlers::screenshots::delete_screenshot)
                .patch(handlers::screenshots::update_screenshot),
        )
        .route("/api/classes", get(handlers::classes::get_classes))
        .route("/api/datagen", post(handlers::datagen::start_datagen))
        .route(
            "/api/datagen/sprites",
            get(handlers::datagen::list_sprite_classes),
        )
        .route(
            "/api/datagen/sprites/{class}/image",
            get(handlers::datagen::get_sprite_image),
        )
        .route(
            "/api/datagen/jobs",
            get(handlers::datagen::list_datagen_jobs),
        )
        .route(
            "/api/datagen/jobs/{id}",
            get(handlers::datagen::get_datagen_job).delete(handlers::datagen::delete_datagen_job),
        )
        .route(
            "/api/datasets",
            get(handlers::datasets::list_datasets).post(handlers::datasets::create_dataset),
        )
        .route(
            "/api/datasets/{name}",
            delete(handlers::datasets::delete_dataset),
        )
        .route("/api/units", get(handlers::units::list_units))
        .route("/api/units/meta", get(handlers::units::units_meta))
        .route("/api/units/{id}", get(handlers::units::get_unit))
        .route(
            "/api/units/{id}/icons",
            get(handlers::units::get_unit_icons),
        )
        .route("/api/icons/sets", get(handlers::icons::list_icon_sets))
        .route(
            "/api/icons/config",
            get(handlers::icons::get_icon_config).put(handlers::icons::put_icon_config),
        )
        .route(
            "/api/icons/classes",
            get(handlers::icons::list_icon_classes),
        )
        .route("/api/icons/units", get(handlers::icons::list_unit_icons))
        .route(
            "/api/icons/sprites/{class}/image",
            get(handlers::icons::get_icon_sprite_image),
        )
        .route(
            "/api/portraits/{id}",
            get(handlers::portraits::get_portrait),
        )
        .route(
            "/ws/training",
            get(handlers::training_ws::training_ws_handler),
        )
        .route(
            "/api/training/status",
            get(handlers::training_ws::get_training_status),
        )
        .route("/api/runs", get(handlers::predict::list_runs))
        .route("/api/predict", post(handlers::predict::predict_json))
        .route(
            "/api/predict/annotate",
            post(handlers::predict::predict_annotate),
        )
}
