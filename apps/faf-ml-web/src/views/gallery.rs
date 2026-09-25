use dioxus::html::{FileData, HasFileData};
use dioxus::prelude::*;
use faf_ml_core::{ScreenshotKind, ScreenshotMeta};
use gloo_net::http::Request;

use crate::Route;

/// Gallery filter chips. `Triage` = uploaded but not yet marked — the
/// "inbox zero" view that keeps the background pool clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GalleryFilter {
    All,
    Triage,
    Battle,
    Background,
    Synthetic,
}

impl GalleryFilter {
    fn matches(self, shot: &ScreenshotMeta) -> bool {
        match self {
            Self::All => true,
            Self::Triage => shot.kind == ScreenshotKind::Unclassified,
            Self::Battle => shot.kind == ScreenshotKind::Battle,
            Self::Background => shot.kind == ScreenshotKind::Background,
            Self::Synthetic => shot.kind == ScreenshotKind::Synthetic,
        }
    }
}

const FILTERS: [(GalleryFilter, &str); 5] = [
    (GalleryFilter::All, "all"),
    (GalleryFilter::Triage, "needs triage"),
    (GalleryFilter::Battle, "battle"),
    (GalleryFilter::Background, "background"),
    (GalleryFilter::Synthetic, "synthetic"),
];

/// Fetch the screenshot index from the server.
async fn fetch_screenshots() -> Result<Vec<ScreenshotMeta>, String> {
    Request::get(&crate::net::api_url("/api/screenshots"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<ScreenshotMeta>>()
        .await
        .map_err(|e| e.to_string())
}

/// Multipart-upload PNG files (kind defaults to `unclassified` server-side —
/// triage happens per-card afterwards).
async fn upload_files(files: Vec<web_sys::File>) -> Result<usize, String> {
    if files.is_empty() {
        return Ok(0);
    }
    let form = web_sys::FormData::new().map_err(|e| format!("{e:?}"))?;
    for file in &files {
        form.append_with_blob_and_filename("files", file, &file.name())
            .map_err(|e| format!("{e:?}"))?;
    }
    let resp = Request::post(&crate::net::api_url("/api/screenshots"))
        .body(form)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.ok() {
        Ok(files.len())
    } else {
        Err(format!("HTTP {}", resp.status()))
    }
}

/// Extract dropped/uploaded files from a Dioxus event (file input and
/// drag-drop both expose `e.files()` as `Vec<FileData>`).
fn collect_files(files: &[FileData]) -> Vec<web_sys::File> {
    files
        .iter()
        .filter_map(|f| f.inner().downcast_ref::<web_sys::File>().cloned())
        .collect()
}

/// Start an upload in the background (signals are Copy, so this plain fn
/// sidesteps closure-move issues between the two call sites).
fn start_upload(files: Vec<web_sys::File>, mut status: Signal<String>, mut refresh: Signal<u32>) {
    spawn(async move {
        match upload_files(files).await {
            Ok(0) => {}
            Ok(n) => status.set(format!(
                "uploaded {n} file(s) — mark each as battle or background below"
            )),
            Err(e) => status.set(format!("upload failed: {e}")),
        }
        *refresh.write() += 1;
    });
}

/// Gallery: drag-and-drop upload, per-card triage (battle/background),
/// filter chips, delete.
#[component]
pub fn Gallery() -> Element {
    // Bump to force the list resource to re-run after an upload/delete/triage.
    let refresh = use_signal(|| 0u32);
    let status = use_signal(String::new);
    let mut filter = use_signal(|| GalleryFilter::Triage);
    let mut dragging = use_signal(|| false);

    let shots = use_resource(move || async move {
        refresh();
        fetch_screenshots().await
    });

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans p-6",
            div { class: "max-w-6xl mx-auto",
                crate::workflow::WorkflowBanner { step: 2 }
                div { class: "flex items-center gap-4 mb-4",
                    h1 { class: "text-2xl font-bold text-white", "Screenshot gallery" }
                    div { class: "flex-1" }
                    label { class: "px-4 py-2 rounded bg-blue-700 hover:bg-blue-600 text-white text-sm font-semibold cursor-pointer transition-colors",
                        "Upload PNGs"
                        input {
                            class: "hidden",
                            r#type: "file",
                            accept: "image/png",
                            multiple: true,
                            onchange: move |e| start_upload(collect_files(&e.files()), status, refresh),
                        }
                    }
                }

                // Drag-and-drop zone.
                div {
                    class: if *dragging.read() { "mb-4 rounded-lg border-2 border-dashed border-blue-500 bg-blue-950/40 p-8 text-center text-blue-300 transition-colors" } else { "mb-4 rounded-lg border-2 border-dashed border-neutral-700 bg-neutral-900 p-8 text-center text-neutral-400 transition-colors" },
                    ondragover: move |e| {
                        e.prevent_default();
                        dragging.set(true);
                    },
                    ondragleave: move |_| dragging.set(false),
                    ondrop: move |e| {
                        e.prevent_default();
                        dragging.set(false);
                        start_upload(collect_files(&e.files()), status, refresh);
                    },
                    "Drop screenshots here — then mark each as "
                    b { "battle" }
                    " (has units, kept for testing) or "
                    b { "background" }
                    " (empty terrain, used to generate training data)."
                }

                if !status.read().is_empty() {
                    p { class: "text-sm text-amber-400 mb-4", "{status}" }
                }

                // Filter chips.
                div { class: "flex gap-2 mb-4 text-xs",
                    for (f , label) in FILTERS {
                        button {
                            class: if *filter.read() == f { "px-3 py-1 rounded bg-neutral-200 text-neutral-900 font-semibold" } else { "px-3 py-1 rounded bg-neutral-800 text-neutral-400 hover:bg-neutral-700" },
                            onclick: move |_| filter.set(f),
                            "{label}"
                        }
                    }
                }

                match &*shots.read() {
                    None => rsx! {
                        p { class: "text-neutral-400", "Loading..." }
                    },
                    Some(Err(e)) => rsx! {
                        p { class: "text-red-400", "{e}" }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p { class: "text-neutral-400", "No screenshots yet — drop some PNGs above." }
                    },
                    Some(Ok(list)) => {
                        let shown: Vec<&ScreenshotMeta> = list
                            .iter()
                            .filter(|s| filter.read().matches(s))
                            .collect();
                        let clear_count = shown.len();
                        rsx! {
                            if *filter.read() == GalleryFilter::Synthetic && clear_count > 0 {
                                div { class: "mb-3",
                                    button {
                                        class: "px-3 py-1.5 rounded bg-red-900/60 hover:bg-red-800 text-red-200 text-xs font-semibold transition-colors",
                                        onclick: move |_| clear_synthetic(clear_count, status, refresh),
                                        "Clear {clear_count} synthetic sample(s)"
                                    }
                                }
                            }
                            if shown.is_empty() {
                                p { class: "text-neutral-400", "Nothing in this view." }
                            }
                            div { class: "grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-4",
                                for shot in shown {
                                    ShotCard { key: "{shot.id}", shot: shot.clone(), refresh }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Bulk-delete every synthetic sample after a confirm dialog (covers
/// samples that predate per-job tracking, which job-row deletion cannot
/// attribute). Free fn for the same reason as `set_kind`.
fn clear_synthetic(count: usize, mut status: Signal<String>, mut refresh: Signal<u32>) {
    let confirmed = web_sys::window()
        .and_then(|w| {
            w.confirm_with_message(&format!(
                "Delete all {count} synthetic sample(s)? This cannot be undone."
            ))
            .ok()
        })
        .unwrap_or(false);
    if !confirmed {
        return;
    }
    spawn(async move {
        let resp = Request::delete(&crate::net::api_url("/api/screenshots?kind=synthetic"))
            .send()
            .await;
        match resp {
            Ok(r) if r.ok() => status.set("cleared synthetic samples".to_string()),
            Ok(r) => status.set(format!("clear failed: HTTP {}", r.status())),
            Err(e) => status.set(format!("clear failed: {e}")),
        }
        *refresh.write() += 1;
    });
}

/// PATCH the screenshot's kind (the triage action), then refresh the list.
/// Free fn rather than a closure: it's called from multiple event handlers.
fn set_kind(id: String, kind: ScreenshotKind, mut refresh: Signal<u32>) {
    spawn(async move {
        let body = format!("{{\"kind\":\"{}\"}}", kind.as_str());
        let req = Request::patch(&crate::net::api_url(&format!("/api/screenshots/{id}")))
            .header("content-type", "application/json")
            .body(body);
        if let Ok(req) = req {
            let _ = req.send().await;
        }
        *refresh.write() += 1;
    });
}

/// One thumbnail card linking to the label view, with the triage toggle
/// (battle/background) and a delete button.
#[component]
fn ShotCard(shot: ScreenshotMeta, refresh: Signal<u32>) -> Element {
    let id = shot.id.to_string();
    let delete_id = id.clone();
    let battle_id = id.clone();
    let background_id = id.clone();
    let dimensions = format!("{}×{}", shot.width, shot.height);
    let uploaded = shot.uploaded_at.format("%Y-%m-%d %H:%M").to_string();
    let (badge_text, badge_class) = match shot.kind {
        ScreenshotKind::Unclassified => ("needs triage", "bg-amber-600 text-amber-50"),
        ScreenshotKind::Battle => ("battle", "bg-red-900 text-red-200"),
        ScreenshotKind::Background => ("background", "bg-green-900 text-green-200"),
        ScreenshotKind::Synthetic => ("synthetic", "bg-blue-900 text-blue-200"),
    };

    rsx! {
        div { class: "rounded-lg border border-neutral-800 bg-neutral-900 overflow-hidden",
            Link {
                to: Route::Label {
                    id: shot.id.to_string(),
                },
                div { class: "relative",
                    img {
                        class: "w-full aspect-video object-cover",
                        src: crate::net::image_url(&shot.id.to_string()),
                        alt: "{shot.filename}",
                    }
                    span { class: "absolute top-1 left-1 px-1.5 py-0.5 rounded text-[10px] font-semibold {badge_class}",
                        "{badge_text}"
                    }
                }
            }
            div { class: "px-3 pt-2",
                p { class: "text-xs text-neutral-300 truncate", "{shot.filename}" }
                p { class: "text-xs text-neutral-500", "{dimensions} · {uploaded}" }
            }
            div { class: "flex items-center gap-1 px-3 py-2 text-[11px]",
                // Triage toggle (synthetic shots are imported, not triaged).
                if shot.kind != ScreenshotKind::Synthetic {
                    for (k , label , kind_id) in [
                        (ScreenshotKind::Battle, "battle", battle_id.clone()),
                        (ScreenshotKind::Background, "background", background_id.clone()),
                    ]
                    {
                        button {
                            class: if shot.kind == k { "px-2 py-1 rounded bg-neutral-200 text-neutral-900 font-semibold" } else { "px-2 py-1 rounded bg-neutral-800 text-neutral-400 hover:bg-neutral-700" },
                            onclick: move |_| set_kind(kind_id.clone(), k, refresh),
                            "{label}"
                        }
                    }
                }
                div { class: "flex-1" }
                button {
                    class: "px-2 py-1 rounded text-red-400 hover:bg-neutral-800 transition-colors",
                    title: "Delete screenshot and its labels",
                    onclick: move |_| {
                        let id = delete_id.clone();
                        spawn(async move {
                            let _ = Request::delete(
                                    &crate::net::api_url(&format!("/api/screenshots/{id}")),
                                )
                                .send()
                                .await;
                            *refresh.write() += 1;
                        });
                    },
                    "Delete"
                }
            }
        }
    }
}
