use dioxus::prelude::*;
use faf_ml_core::{DatasetManifest, ScreenshotMeta};
use gloo_net::http::Request;
use serde::Serialize;

/// Fetch all dataset snapshot manifests.
async fn fetch_datasets() -> Result<Vec<DatasetManifest>, String> {
    Request::get(&crate::net::api_url("/api/datasets"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<DatasetManifest>>()
        .await
        .map_err(|e| e.to_string())
}

/// Fetch the screenshot index (counts per kind drive the snapshot form).
async fn fetch_screenshots() -> Result<Vec<ScreenshotMeta>, String> {
    Request::get(&crate::net::api_url("/api/screenshots"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<ScreenshotMeta>>()
        .await
        .map_err(|e| e.to_string())
}

/// Request body for `POST /api/datasets` (kind filter; the server resolves
/// the image ids from the screenshot index).
#[derive(Serialize)]
struct CreateDatasetRequest {
    name: String,
    kinds: Vec<String>,
}

/// The pools a snapshot can draw from, with a hint for each.
const KIND_CHOICES: [(&str, &str); 4] = [
    ("synthetic", "datagen output — auto-labeled training data"),
    ("battle", "real shots — only useful after label correction"),
    ("background", "empty terrain — no labels, rarely wanted"),
    (
        "unclassified",
        "untriaged uploads — no labels, rarely wanted",
    ),
];

/// Datasets: list immutable snapshots; create one from a chosen set of
/// screenshot kinds (labels are embedded at snapshot time).
#[component]
pub fn Datasets() -> Element {
    let mut refresh = use_signal(|| 0u32);
    let mut name = use_signal(String::new);
    let mut status = use_signal(String::new);
    // Kinds included in the next snapshot; synthetic is the sensible default.
    let mut include: Signal<std::collections::HashSet<&'static str>> =
        use_signal(|| std::collections::HashSet::from(["synthetic"]));

    let datasets = use_resource(move || async move {
        refresh();
        fetch_datasets().await
    });
    let shots = use_resource(move || async move {
        refresh();
        fetch_screenshots().await
    });

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans p-6",
            div { class: "max-w-4xl mx-auto",
                crate::workflow::WorkflowBanner { step: 5 }
                h1 { class: "text-2xl font-bold text-white mb-6", "Dataset snapshots" }

                // Create form: name + kind selection + snapshot button.
                div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-4 mb-6",
                    h2 { class: "text-sm font-semibold text-white mb-3", "Create snapshot" }
                    p { class: "text-xs text-neutral-400 mb-3",
                        "Choose which screenshot pools to freeze into the snapshot. "
                        "For detector training, "
                        b { "synthetic" }
                        " alone is usually right — battle shots stay held out unless "
                        "you corrected their labels."
                    }
                    match &*shots.read() {
                        None => rsx! { p { class: "text-xs text-neutral-500 mb-3", "Loading screenshots..." } },
                        Some(Err(e)) => rsx! { p { class: "text-xs text-red-400 mb-3", "{e}" } },
                        Some(Ok(metas)) => {
                            let selected = metas
                                .iter()
                                .filter(|m| include.read().contains(m.kind.as_str()))
                                .count();
                            rsx! {
                                div { class: "grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1.5 mb-3",
                                    for (kind, hint) in KIND_CHOICES {
                                        {
                                            let count = metas
                                                .iter()
                                                .filter(|m| m.kind.as_str() == kind)
                                                .count();
                                            let checked = include.read().contains(kind);
                                            rsx! {
                                                label { key: "{kind}", class: "flex items-center gap-2 text-xs text-neutral-300 cursor-pointer",
                                                    input {
                                                        r#type: "checkbox",
                                                        class: "accent-blue-500",
                                                        checked: checked,
                                                        onchange: move |_| {
                                                            let mut set = include.write();
                                                            if !set.remove(kind) {
                                                                set.insert(kind);
                                                            }
                                                        },
                                                    }
                                                    span { class: "font-mono", "{kind}" }
                                                    span { class: "text-neutral-500", "({count}) — {hint}" }
                                                }
                                            }
                                        }
                                    }
                                }
                                div { class: "flex gap-2",
                                    input {
                                        class: "flex-1 px-3 py-2 rounded bg-neutral-800 border border-neutral-700 text-sm text-white",
                                        placeholder: "dataset name (e.g. v1)",
                                        value: "{name}",
                                        oninput: move |e| name.set(e.value()),
                                    }
                                    button {
                                        class: "px-4 py-2 rounded bg-blue-700 hover:bg-blue-600 disabled:opacity-40 disabled:hover:bg-blue-700 text-white text-sm font-semibold transition-colors",
                                        disabled: selected == 0,
                                        onclick: move |_| {
                                            let dataset_name = name.read().trim().to_string();
                                            if dataset_name.is_empty() {
                                                status.set("name is required".to_string());
                                                return;
                                            }
                                            let kinds: Vec<String> =
                                                include.read().iter().map(|k| k.to_string()).collect();
                                            spawn(async move {
                                                let resp = Request::post(&crate::net::api_url("/api/datasets"))
                                                    .json(&CreateDatasetRequest { name: dataset_name, kinds })
                                                    .map_err(|e| e.to_string())
                                                    .map(|r| r.send());
                                                let result = match resp {
                                                    Ok(fut) => fut.await.map_err(|e| e.to_string()),
                                                    Err(e) => Err(e),
                                                };
                                                match result {
                                                    Ok(resp) if resp.ok() => {
                                                        status.set("snapshot created".to_string());
                                                        name.set(String::new());
                                                    }
                                                    Ok(resp) => {
                                                        status.set(format!("create failed: HTTP {}", resp.status()));
                                                    }
                                                    Err(e) => status.set(format!("create failed: {e}")),
                                                }
                                                *refresh.write() += 1;
                                            });
                                        },
                                        "Snapshot {selected} images"
                                    }
                                }
                            }
                        }
                    }
                    if !status.read().is_empty() {
                        p { class: "text-xs text-amber-400 mt-2", "{status}" }
                    }
                }

                match &*datasets.read() {
                    None => rsx! { p { class: "text-neutral-400", "Loading..." } },
                    Some(Err(e)) => rsx! { p { class: "text-red-400", "{e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p { class: "text-neutral-400", "No datasets yet." }
                    },
                    Some(Ok(list)) => rsx! {
                        div { class: "space-y-2",
                            for ds in list.iter() {
                                DatasetRow { key: "{ds.name}", ds: ds.clone(), refresh }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// `DELETE /api/datasets/{name}` after a confirm dialog, then refresh.
fn delete_dataset(name: String, mut refresh: Signal<u32>) {
    let confirmed = web_sys::window()
        .and_then(|w| {
            w.confirm_with_message(&format!(
                "Delete snapshot {name:?}? The file is removed permanently."
            ))
            .ok()
        })
        .unwrap_or(false);
    if !confirmed {
        return;
    }
    spawn(async move {
        let _ = Request::delete(&crate::net::api_url(&format!("/api/datasets/{name}")))
            .send()
            .await;
        *refresh.write() += 1;
    });
}

/// One dataset row: name, image/box counts, creation date, delete button.
#[component]
fn DatasetRow(ds: DatasetManifest, refresh: Signal<u32>) -> Element {
    let images = ds.entries.len();
    let boxes: usize = ds.entries.iter().map(|e| e.labels.len()).sum();
    let created = ds.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let name = ds.name.clone();
    rsx! {
        div { class: "flex items-center gap-4 rounded-lg border border-neutral-800 bg-neutral-900 px-4 py-3",
            span { class: "font-mono text-white", "{ds.name}" }
            div { class: "flex-1" }
            span { class: "text-sm text-neutral-400", "{images} images · {boxes} boxes" }
            span { class: "text-xs text-neutral-500", "{created}" }
            button {
                class: "px-2 py-1 rounded text-xs text-red-400 hover:bg-neutral-800 transition-colors",
                title: "Delete this snapshot",
                onclick: move |_| delete_dataset(name.clone(), refresh),
                "Delete"
            }
        }
    }
}
