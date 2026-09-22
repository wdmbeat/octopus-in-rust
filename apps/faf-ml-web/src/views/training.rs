use dioxus::prelude::*;
use faf_dioxus_ui::{ChartMetric, ChartSeries, ChartTab, RGBColor, UplotChart};
use faf_ml_core::{
    TrainingCommand, TrainingConfig, TrainingEvent, TrainingMetricsPoint, TrainingRunStatus,
    TrainingStatus,
};
use gloo_net::http::Request;
use uuid::Uuid;

use crate::components::TrainingConnection;

/// Fetch the current/last training run (404 when none ever ran).
async fn fetch_status() -> Result<TrainingRunStatus, String> {
    let resp = Request::get(&crate::net::api_url("/api/training/status"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == 404 {
        return Err("no training run yet".to_string());
    }
    resp.json::<TrainingRunStatus>()
        .await
        .map_err(|e| e.to_string())
}

/// Fetch dataset snapshot names (training requires one — step 5).
async fn fetch_datasets() -> Result<Vec<faf_ml_core::DatasetManifest>, String> {
    Request::get(&crate::net::api_url("/api/datasets"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<faf_ml_core::DatasetManifest>>()
        .await
        .map_err(|e| e.to_string())
}

/// Page lifecycle; drives which controls are enabled (mirrors fafcn's
/// `SimulationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageStatus {
    Idle,
    Running,
    /// Pause accepted; waiting for the training thread to reach the batch
    /// boundary.
    Pausing,
    Paused,
    /// Stop accepted; waiting for the terminal event.
    Stopping,
    Finished,
}

// ── chart extractors (fn pointers for `ChartMetric`) ────────────────────────

fn x_seq(p: &TrainingMetricsPoint) -> f64 {
    p.seq as f64
}
fn y_train(p: &TrainingMetricsPoint) -> f64 {
    p.train_loss
}
fn y_cls(p: &TrainingMetricsPoint) -> f64 {
    p.cls_loss
}
fn y_bbox(p: &TrainingMetricsPoint) -> f64 {
    p.bbox_loss
}
/// `None` → NaN: uPlot leaves a gap, so epoch-eval series appear as points.
fn y_valid(p: &TrainingMetricsPoint) -> f64 {
    p.valid_loss.unwrap_or(f64::NAN)
}
fn y_map(p: &TrainingMetricsPoint) -> f64 {
    p.map.unwrap_or(f64::NAN)
}

/// Parse the config fields (empty = keep default).
fn parse_config(
    dataset: &str,
    epochs: &str,
    batch_size: &str,
    lr: &str,
    valid_fraction: &str,
    cpu: bool,
) -> Result<TrainingConfig, String> {
    let mut config = TrainingConfig::default();
    if dataset.trim().is_empty() {
        return Err("pick a dataset snapshot first (create one on the Datasets page)".to_string());
    }
    config.dataset = dataset.to_string();
    let raw = epochs.trim();
    if !raw.is_empty() {
        config.epochs = raw
            .parse()
            .map_err(|_| format!("invalid epochs: {raw:?}"))?;
    }
    let raw = batch_size.trim();
    if !raw.is_empty() {
        config.batch_size = raw
            .parse()
            .map_err(|_| format!("invalid batch size: {raw:?}"))?;
    }
    let raw = lr.trim();
    if !raw.is_empty() {
        config.lr = raw.parse().map_err(|_| format!("invalid lr: {raw:?}"))?;
    }
    let raw = valid_fraction.trim();
    if !raw.is_empty() {
        config.valid_fraction = raw
            .parse()
            .map_err(|_| format!("invalid valid fraction: {raw:?}"))?;
    }
    config.cpu = cpu;
    Ok(config)
}

/// How `connect` opens the socket: start a new run (the server assigns the
/// id via `TrainingEvent::Started`) or attach to an existing one (id from
/// `GET /api/training/status`).
enum ConnectMode {
    Start { config: TrainingConfig, speed: f64 },
    Attach { id: Uuid },
}

/// Wire the metrics/status handlers and open a connection (see `ConnectMode`).
fn connect(
    mode: ConnectMode,
    mut connection: Signal<Option<TrainingConnection>>,
    mut status: Signal<PageStatus>,
    mut detail: Signal<String>,
    mut data: Signal<Vec<TrainingMetricsPoint>>,
    mut latest: Signal<Option<TrainingMetricsPoint>>,
) {
    let on_message = move |msg: TrainingEvent| match msg {
        TrainingEvent::Metrics { point, .. } => {
            latest.set(Some(point.clone()));
            data.write().push(point);
        }
        TrainingEvent::Status {
            status: run_status, ..
        } => match run_status {
            TrainingStatus::Running => {
                status.set(PageStatus::Running);
            }
            TrainingStatus::Pausing => {
                status.set(PageStatus::Pausing);
            }
            TrainingStatus::Paused => {
                status.set(PageStatus::Paused);
            }
            TrainingStatus::Stopping => {
                status.set(PageStatus::Stopping);
            }
            TrainingStatus::Done {
                run_dir,
                duration_secs,
            } => {
                status.set(PageStatus::Finished);
                detail.set(format!("done in {duration_secs}s → {run_dir}"));
            }
            TrainingStatus::Stopped {
                run_dir,
                duration_secs,
            } => {
                status.set(PageStatus::Finished);
                detail.set(format!(
                    "stopped after {duration_secs}s — checkpoint saved → {run_dir}"
                ));
            }
            TrainingStatus::Failed { error } => {
                status.set(PageStatus::Finished);
                detail.set(format!("failed: {error}"));
            }
        },
        TrainingEvent::Cleared { .. } => {
            data.write().clear();
            latest.set(None);
            status.set(PageStatus::Idle);
            detail.set("run reset".to_string());
        }
        // `Started`/`Finished`/`Error` are handled by TrainingConnection.
        TrainingEvent::Started { .. }
        | TrainingEvent::Finished { .. }
        | TrainingEvent::Error { .. } => {}
    };
    let on_status = move |text: String| {
        if text == "finished" {
            status.set(PageStatus::Finished);
        } else if text != "connected" {
            detail.set(text);
        }
    };
    let result = match mode {
        ConnectMode::Start { config, speed } => {
            TrainingConnection::open(config, speed, on_message, on_status)
        }
        ConnectMode::Attach { id } => TrainingConnection::open_attach(id, on_message, on_status),
    };
    match result {
        Ok(conn) => connection.set(Some(conn)),
        Err(e) => detail.set(format!("failed to connect: {e:?}")),
    }
}

/// Training monitor: start/pause/resume/stop/reset a training run and watch
/// its metrics live. On load it checks the server registry — if a run is active
/// it attaches (the replay refills the charts); a finished run shows its
/// outcome. Mirrors fafcn's simulate page: a WebSocket streams events into a
/// signal, uPlot renders the buffer.
#[component]
pub fn Training() -> Element {
    let mut status = use_signal(|| PageStatus::Idle);
    let mut detail = use_signal(String::new);
    let connection = use_signal(|| None::<TrainingConnection>);
    let mut data: Signal<Vec<TrainingMetricsPoint>> = use_signal(Vec::new);
    let mut latest: Signal<Option<TrainingMetricsPoint>> = use_signal(|| None);

    let defaults = TrainingConfig::default();
    let epochs = use_signal(|| defaults.epochs.to_string());
    let batch_size = use_signal(|| defaults.batch_size.to_string());
    let mut lr = use_signal(|| defaults.lr.to_string());
    let valid_fraction = use_signal(|| defaults.valid_fraction.to_string());
    let mut cpu = use_signal(|| false);
    let speed = use_signal(|| "0".to_string());
    let speed_value = move || speed.read().parse::<f64>().unwrap_or(0.0);

    // Dataset snapshots (training requires one); selection defaults to the
    // first available snapshot.
    let datasets = use_resource(fetch_datasets);
    let mut dataset = use_signal(String::new);
    let mut dataset_initialized = use_signal(|| false);
    use_effect(move || {
        if *dataset_initialized.read() {
            return;
        }
        if let Some(Ok(list)) = datasets.read().as_ref() {
            if let Some(first) = list.first() {
                dataset.set(first.name.clone());
            }
            dataset_initialized.set(true);
        }
    });

    // On mount: attach to an active run (replay refills the charts) or show
    // the last run's outcome. Runs once (attached flag).
    let status_res = use_resource(fetch_status);
    let mut attached = use_signal(|| false);
    use_effect(move || {
        if *attached.read() {
            return;
        }
        let run = status_res
            .read()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned();
        let Some(run) = run else {
            return;
        };
        attached.set(true);
        match run.status {
            TrainingStatus::Running | TrainingStatus::Pausing | TrainingStatus::Stopping => {
                status.set(match run.status {
                    TrainingStatus::Pausing => PageStatus::Pausing,
                    TrainingStatus::Stopping => PageStatus::Stopping,
                    _ => PageStatus::Running,
                });
                detail.set("attached to the running job — replaying metrics".to_string());
                connect(
                    ConnectMode::Attach { id: run.id },
                    connection,
                    status,
                    detail,
                    data,
                    latest,
                );
            }
            TrainingStatus::Paused => {
                status.set(PageStatus::Paused);
                detail.set("attached to the paused job — replaying metrics".to_string());
                connect(
                    ConnectMode::Attach { id: run.id },
                    connection,
                    status,
                    detail,
                    data,
                    latest,
                );
            }
            TrainingStatus::Done {
                run_dir,
                duration_secs,
            } => {
                detail.set(format!("last run: done in {duration_secs}s → {run_dir}"));
            }
            TrainingStatus::Stopped {
                run_dir,
                duration_secs,
            } => {
                detail.set(format!(
                    "last run: stopped after {duration_secs}s → {run_dir}"
                ));
            }
            TrainingStatus::Failed { error } => {
                detail.set(format!("last run: failed — {error}"));
            }
        }
    });

    // Forward speed changes to a running/paused job (mirrors the sim page).
    use_effect(move || {
        let s = speed_value();
        if matches!(*status.read(), PageStatus::Running | PageStatus::Paused) {
            if let Some(conn) = connection.read().as_ref() {
                conn.send_command(|id| TrainingCommand::SetSpeed {
                    id,
                    batches_per_sec: s,
                });
            }
        }
    });

    let start = move |_| {
        let config = match parse_config(
            &dataset.read(),
            &epochs.read(),
            &batch_size.read(),
            &lr.read(),
            &valid_fraction.read(),
            *cpu.read(),
        ) {
            Ok(config) => config,
            Err(e) => {
                detail.set(e);
                return;
            }
        };
        data.write().clear();
        latest.set(None);
        detail.set(String::new());
        connect(
            ConnectMode::Start {
                config,
                speed: speed_value(),
            },
            connection,
            status,
            detail,
            data,
            latest,
        );
    };

    let send = move |build: fn(Uuid) -> TrainingCommand| {
        if let Some(conn) = connection.read().as_ref() {
            conn.send_command(build);
        }
    };

    let reset = move |_| send(|id| TrainingCommand::Reset { id });

    let can_start = matches!(*status.read(), PageStatus::Idle | PageStatus::Finished);
    let can_pause = matches!(*status.read(), PageStatus::Running);
    let can_resume = matches!(*status.read(), PageStatus::Pausing | PageStatus::Paused);
    let can_stop = matches!(
        *status.read(),
        PageStatus::Running | PageStatus::Pausing | PageStatus::Paused
    );
    let can_reset = !matches!(*status.read(), PageStatus::Idle);
    let (badge_text, badge_class) = match *status.read() {
        PageStatus::Idle => ("idle", "bg-neutral-800 text-neutral-400"),
        PageStatus::Running => ("running", "bg-blue-900 text-blue-200"),
        PageStatus::Pausing => ("pausing…", "bg-amber-900 text-amber-200"),
        PageStatus::Paused => ("paused", "bg-amber-900 text-amber-200"),
        PageStatus::Stopping => ("stopping…", "bg-amber-900 text-amber-200"),
        PageStatus::Finished => ("finished", "bg-green-900 text-green-200"),
    };

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans p-6",
            div { class: "max-w-6xl mx-auto",
                crate::workflow::WorkflowBanner { step: 6 }
                div { class: "flex items-center gap-3 mb-4",
                    h1 { class: "text-2xl font-bold text-white", "Training monitor" }
                    span { class: "px-2 py-0.5 rounded text-xs font-semibold {badge_class}", "{badge_text}" }
                }

                // Controls.
                div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-4 mb-4",
                    p { class: "text-xs text-neutral-400 mb-3",
                        "Runs a real SSD training job on the server (Wgpu/Vulkan, or CPU) on the "
                        "selected dataset snapshot — snapshots are immutable, so runs reproduce "
                        "(create one on the Datasets page first). Metrics stream over a WebSocket; "
                        "the job lives server-side: this page can be closed and re-opened — it "
                        "re-attaches and replays."
                    }
                    div { class: "mb-4",
                        match &*datasets.read() {
                            None => rsx! { p { class: "text-xs text-neutral-500", "Loading snapshots..." } },
                            Some(Err(e)) => rsx! { p { class: "text-xs text-red-400", "{e}" } },
                            Some(Ok(list)) if list.is_empty() => rsx! {
                                p { class: "text-xs text-amber-400",
                                    "No dataset snapshots yet — training requires one. Create it in step 5: "
                                    Link { class: "text-blue-400 hover:underline", to: crate::Route::Datasets {}, "Datasets" }
                                }
                            },
                            Some(Ok(list)) => rsx! {
                                label { class: "flex items-center gap-2 text-xs text-neutral-400",
                                    span { "dataset snapshot" }
                                    select {
                                        class: "px-3 py-1.5 rounded bg-neutral-800 border border-neutral-700 text-sm text-white",
                                        value: "{dataset}",
                                        onchange: move |e| dataset.set(e.value()),
                                        for d in list.iter() {
                                            option {
                                                key: "{d.name}",
                                                value: "{d.name}",
                                                "{d.name} ({d.entries.len()} images)"
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                    div { class: "grid grid-cols-2 md:grid-cols-3 gap-x-6 gap-y-3 mb-4",
                        SliderField { label: "epochs", min: "1", max: "100", step: "1", value: epochs }
                        SliderField { label: "batch size (GPU cap: 4)", min: "1", max: "4", step: "1", value: batch_size }
                        SliderField { label: "valid fraction", min: "0", max: "0.3", step: "0.05", value: valid_fraction }
                        SliderField { label: "speed (batches/s, 0=unlimited)", min: "0", max: "50", step: "1", value: speed }
                        label { class: "flex flex-col gap-1 text-xs text-neutral-400",
                            div { class: "flex items-center justify-between",
                                span { "learning rate" }
                                span { class: "font-mono text-sm text-neutral-100", "{lr}" }
                            }
                            input {
                                class: "px-3 py-1.5 rounded bg-neutral-800 border border-neutral-700 text-sm text-white",
                                value: "{lr}",
                                oninput: move |e| lr.set(e.value()),
                            }
                        }
                        label { class: "flex items-end gap-2 pb-2 text-xs text-neutral-400 cursor-pointer",
                            input {
                                r#type: "checkbox",
                                class: "accent-blue-500",
                                checked: *cpu.read(),
                                onchange: move |e| cpu.set(e.value().parse().unwrap_or(false)),
                            }
                            "cpu backend (slow, no GPU)"
                        }
                    }
                    div { class: "flex items-center gap-2",
                        button {
                            class: "px-4 py-2 rounded bg-blue-700 hover:bg-blue-600 disabled:opacity-40 disabled:hover:bg-blue-700 text-white text-sm font-semibold transition-colors",
                            disabled: !can_start,
                            onclick: start,
                            "Start"
                        }
                        button {
                            class: "px-3 py-2 rounded bg-neutral-800 hover:bg-neutral-700 disabled:opacity-40 text-neutral-200 text-sm transition-colors",
                            disabled: !can_pause,
                            onclick: move |_| send(|id| TrainingCommand::Pause { id }),
                            "Pause"
                        }
                        button {
                            class: "px-3 py-2 rounded bg-neutral-800 hover:bg-neutral-700 disabled:opacity-40 text-neutral-200 text-sm transition-colors",
                            disabled: !can_resume,
                            onclick: move |_| send(|id| TrainingCommand::Resume { id }),
                            "Resume"
                        }
                        button {
                            class: "px-3 py-2 rounded bg-red-900/60 hover:bg-red-800 disabled:opacity-40 text-red-200 text-sm transition-colors",
                            disabled: !can_stop,
                            onclick: move |_| send(|id| TrainingCommand::Stop { id }),
                            "Stop"
                        }
                        button {
                            class: "px-3 py-2 rounded bg-red-900/60 hover:bg-red-800 disabled:opacity-40 text-red-200 text-sm transition-colors",
                            disabled: !can_reset,
                            onclick: reset,
                            "Reset"
                        }
                        if let Some(point) = latest.read().as_ref() {
                            span { class: "ml-2 text-xs text-neutral-400 tabular-nums",
                                "epoch {point.epoch} · batch {point.batch} · train {point.train_loss:.4} · cls {point.cls_loss:.4} · bbox {point.bbox_loss:.4}"
                            }
                        }
                        if !detail.read().is_empty() {
                            span { class: "ml-2 text-xs text-amber-400", "{detail}" }
                        }
                    }
                }

                // Charts (stacked so each gets the full page width).
                div { class: "flex flex-col gap-4",
                    div {
                        h2 { class: "text-sm font-semibold text-white mb-2", "Loss" }
                        // Sized flex parent: UplotChart's root is `flex-1`, which
                        // only gets height inside a flex container (its own
                        // border/background is built in).
                        div { class: "h-96 flex flex-col",
                            UplotChart {
                                data,
                                x_extractor: ChartMetric::new(x_seq),
                                tabs: vec![ChartTab {
                                    label: "loss".to_string(),
                                    series: vec![
                                        ChartSeries::new("train", RGBColor(240, 240, 240), ChartMetric::new(y_train)),
                                        ChartSeries::new("cls", RGBColor(96, 165, 250), ChartMetric::new(y_cls)),
                                        ChartSeries::new("bbox", RGBColor(251, 146, 60), ChartMetric::new(y_bbox)),
                                        ChartSeries {
                                            label: "valid".to_string(),
                                            color: RGBColor(74, 222, 128),
                                            y_extractor: ChartMetric::new(y_valid),
                                            dash: Some(vec![4.0, 4.0]),
                                            span_gaps: true,
                                        },
                                    ],
                                }],
                            }
                        }
                    }
                    div {
                        h2 { class: "text-sm font-semibold text-white mb-2", "mAP (dummy)" }
                        div { class: "h-96 flex flex-col",
                            UplotChart {
                                data,
                                x_extractor: ChartMetric::new(x_seq),
                                tabs: vec![ChartTab {
                                    label: "mAP".to_string(),
                                    series: vec![ChartSeries::new(
                                        "mAP",
                                        RGBColor(192, 132, 252),
                                        ChartMetric::new(y_map),
                                    )
                                    .with_span_gaps()],
                                }],
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One labeled slider (string signal; mirrors the datagen form).
#[component]
fn SliderField(
    label: &'static str,
    min: &'static str,
    max: &'static str,
    step: &'static str,
    mut value: Signal<String>,
) -> Element {
    rsx! {
        label { class: "flex flex-col gap-1 text-xs text-neutral-400",
            div { class: "flex items-center justify-between",
                span { "{label}" }
                span { class: "font-mono text-sm text-neutral-100", "{value}" }
            }
            input {
                r#type: "range",
                class: "w-full accent-blue-500",
                min: "{min}",
                max: "{max}",
                step: "{step}",
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
        }
    }
}
