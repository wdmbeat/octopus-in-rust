use dioxus::prelude::*;
use faf_ml_core::{LabeledBox, ScreenshotMeta};
use gloo_net::http::Request;

/// Fetch the box list for one screenshot.
async fn fetch_labels(id: &str) -> Result<Vec<LabeledBox>, String> {
    Request::get(&crate::net::api_url(&format!(
        "/api/screenshots/{id}/labels"
    )))
    .send()
    .await
    .map_err(|e| e.to_string())?
    .json::<Vec<LabeledBox>>()
    .await
    .map_err(|e| e.to_string())
}

/// Fetch the class list.
async fn fetch_classes() -> Result<Vec<String>, String> {
    Request::get(&crate::net::api_url("/api/classes"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<String>>()
        .await
        .map_err(|e| e.to_string())
}

/// Fetch this screenshot's metadata (natural dimensions for the SVG viewBox).
///
/// NB: reading `naturalWidth` off the DOM via `as_web_event()` is not an
/// option — dioxus-web 0.7.9's `WebImageEvent::as_any` returns the raw event,
/// so `as_web_event()` always panics on image load events.
async fn fetch_meta(id: &str) -> Result<ScreenshotMeta, String> {
    let shots = Request::get(&crate::net::api_url("/api/screenshots"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<ScreenshotMeta>>()
        .await
        .map_err(|e| e.to_string())?;
    shots
        .into_iter()
        .find(|s| s.id.to_string() == id)
        .ok_or_else(|| "screenshot not found".to_string())
}

/// Label view: image with an SVG overlay of the existing bounding boxes.
///
/// Boxes live in absolute pixel coordinates of the natural image; the SVG
/// `viewBox` matches the natural size so the overlay scales exactly with the
/// displayed image (no canvas, no manual coordinate math). Interactions are
/// review-only: select a box, re-assign its class, delete it, save.
#[component]
pub fn Label(id: String) -> Element {
    let mut boxes: Signal<Vec<LabeledBox>> = use_signal(Vec::new);
    let mut loaded = use_signal(|| false);
    let mut selected: Signal<Option<usize>> = use_signal(|| None);
    let mut status = use_signal(String::new);

    let labels_res = use_resource({
        let id = id.clone();
        move || {
            let id = id.clone();
            async move { fetch_labels(&id).await }
        }
    });
    let classes_res = use_resource(fetch_classes);
    let meta_res = use_resource({
        let id = id.clone();
        move || {
            let id = id.clone();
            async move { fetch_meta(&id).await }
        }
    });

    // Copy fetched labels into the editable signal exactly once.
    use_effect(move || {
        if *loaded.read() {
            return;
        }
        if let Some(Ok(labels)) = labels_res.read().as_ref() {
            boxes.set(labels.clone());
            loaded.set(true);
        }
    });

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans p-6",
            div { class: "max-w-7xl mx-auto",
                crate::workflow::WorkflowBanner { step: 4 }
                h1 { class: "text-2xl font-bold text-white mb-4", "Review labels" }
                match &*labels_res.read() {
                    None => rsx! { p { class: "text-neutral-400", "Loading..." } },
                    Some(Err(e)) => rsx! { p { class: "text-red-400", "{e}" } },
                    Some(Ok(_)) => rsx! {
                        div { class: "flex gap-6 items-start",
                            // Image + SVG overlay.
                            div { class: "relative flex-1 min-w-0 select-none",
                                img {
                                    class: "w-full h-auto block",
                                    src: crate::net::image_url(&id),
                                    alt: "screenshot",
                                }
                                if let Some((nw, nh)) = meta_res
                                    .read()
                                    .as_ref()
                                    .and_then(|r| r.as_ref().ok())
                                    .map(|m| (m.width, m.height))
                                {
                                    svg {
                                        class: "absolute inset-0 w-full h-full",
                                        view_box: "0 0 {nw} {nh}",
                                        for (i, b) in boxes.read().iter().enumerate() {
                                            g { key: "{i}",
                                                rect {
                                                    x: "{b.x}",
                                                    y: "{b.y}",
                                                    width: "{b.w}",
                                                    height: "{b.h}",
                                                    fill: if *selected.read() == Some(i) {
                                                        "rgba(251, 191, 36, 0.25)"
                                                    } else {
                                                        "rgba(74, 222, 128, 0.12)"
                                                    },
                                                    stroke: if *selected.read() == Some(i) {
                                                        "#fbbf24"
                                                    } else {
                                                        "#4ade80"
                                                    },
                                                    stroke_width: "{nw as f32 / 320.0}",
                                                    class: "cursor-pointer",
                                                    onclick: move |_| selected.set(Some(i)),
                                                }
                                                text {
                                                    x: "{b.x}",
                                                    y: "{b.y - nw as f32 / 160.0}",
                                                    fill: if *selected.read() == Some(i) {
                                                        "#fbbf24"
                                                    } else {
                                                        "#4ade80"
                                                    },
                                                    font_size: "{nw as f32 / 64.0}",
                                                    class: "pointer-events-none",
                                                    "{b.class}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // Side panel.
                            div { class: "w-72 shrink-0 rounded-lg border border-neutral-800 bg-neutral-900 p-4",
                                h2 { class: "text-sm font-semibold text-white mb-3",
                                    "{boxes.read().len()} box(es)"
                                }
                                match *selected.read() {
                                    None => rsx! {
                                        p { class: "text-sm text-neutral-400", "Click a box to select it." }
                                    },
                                    Some(i) if i >= boxes.read().len() => rsx! {
                                        p { class: "text-sm text-neutral-400", "Click a box to select it." }
                                    },
                                    Some(i) => rsx! {
                                        p { class: "text-xs text-neutral-500 mb-2",
                                            "x={boxes.read()[i].x:.0} y={boxes.read()[i].y:.0} w={boxes.read()[i].w:.0} h={boxes.read()[i].h:.0}"
                                        }
                                        label { class: "block text-xs text-neutral-400 mb-1", "Class" }
                                        select {
                                            class: "w-full mb-3 px-2 py-1.5 rounded bg-neutral-800 border border-neutral-700 text-sm text-white",
                                            value: "{boxes.read()[i].class}",
                                            onchange: move |e| {
                                                if let Some(b) = boxes.write().get_mut(i) {
                                                    b.class = e.value();
                                                }
                                            },
                                            for class in classes_res.read().as_ref()
                                                .and_then(|r| r.as_ref().ok())
                                                .cloned()
                                                .unwrap_or_default()
                                                .iter()
                                            {
                                                option { value: "{class}", "{class}" }
                                            }
                                        }
                                        button {
                                            class: "w-full px-3 py-1.5 rounded bg-red-900/60 hover:bg-red-800 text-red-200 text-sm transition-colors",
                                            onclick: move |_| {
                                                boxes.write().remove(i);
                                                selected.set(None);
                                            },
                                            "Delete box"
                                        }
                                    },
                                }
                                hr { class: "my-4 border-neutral-800" }
                                button {
                                    class: "w-full px-3 py-2 rounded bg-blue-700 hover:bg-blue-600 text-white text-sm font-semibold transition-colors",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| {
                                            let payload = boxes.read().clone();
                                            let id = id.clone();
                                            spawn(async move {
                                                let response = Request::put(&crate::net::api_url(
                                                        &format!("/api/screenshots/{id}/labels"),
                                                    ))
                                                    .json(&payload)
                                                    .map_err(|e| e.to_string())
                                                    .unwrap()
                                                    .send()
                                                    .await;
                                                match response {
                                                    Ok(resp) if resp.ok() => {
                                                        status.set("saved".to_string());
                                                    }
                                                    Ok(resp) => {
                                                        status.set(format!("save failed: HTTP {}", resp.status()));
                                                    }
                                                    Err(e) => status.set(format!("save failed: {e}")),
                                                }
                                            });
                                        }
                                    },
                                    "Save labels"
                                }
                                if !status.read().is_empty() {
                                    p { class: "text-xs text-amber-400 mt-2", "{status}" }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}
