use std::collections::HashSet;

use dioxus::prelude::*;
use gloo_net::http::Request;
use serde::Deserialize;

use crate::components::{ComparisonPanel, UnitSelector, UnitSummary};

/// Unit database metadata sent by `/api/units/meta` (version + attribution).
#[derive(Clone, Deserialize, PartialEq)]
struct UnitsMeta {
    version: String,
    unit_count: usize,
    source_name: String,
    source_url: String,
}

/// Unit comparison page body: multi-select unit grid + comparison panel.
///
/// Note: this component is exclusive to the Units page — the shared
/// `UnitSelector` stays single-select for other pages; only the selection
/// *handling* here is multi-select (click toggles).
#[component]
pub fn UnitBrowser() -> Element {
    let units = use_resource(move || async move {
        Request::get(&crate::net::api_url("/api/units"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<Vec<UnitSummary>>()
            .await
            .map_err(|e| e.to_string())
    });
    let mut selected = use_signal(Vec::<UnitSummary>::new);
    let meta = use_resource(move || async move {
        Request::get(&crate::net::api_url("/api/units/meta"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<UnitsMeta>()
            .await
            .map_err(|e| e.to_string())
    });

    let unit_list = match units.read().as_ref() {
        Some(Ok(list)) => list.clone(),
        Some(Err(err)) => {
            return rsx! {
                div { class: "flex items-center justify-center h-full text-red-400",
                    "Failed to load units: {err}"
                }
            };
        }
        None => {
            return rsx! {
                div { class: "flex items-center justify-center h-full text-neutral-400",
                    "Loading..."
                }
            };
        }
    };

    let selected_ids: HashSet<String> = selected.read().iter().map(|u| u.id.clone()).collect();

    rsx! {
        div { class: "flex flex-col h-full bg-neutral-950 text-gray-200 overflow-hidden font-sans select-none",
            div { class: "flex flex-1 min-h-0 overflow-hidden",
                div { class: "flex-1 overflow-hidden",
                    UnitSelector {
                        units: unit_list,
                        selected: selected_ids,
                        on_select: move |unit: UnitSummary| {
                            selected.with_mut(|list| {
                                if let Some(pos) = list.iter().position(|u| u.id == unit.id) {
                                    list.remove(pos);
                                } else {
                                    list.push(unit);
                                }
                            });
                        },
                    }
                }
                div { class: "w-96 xl:w-[30rem] 2xl:w-[36rem] shrink-0 border-l border-neutral-800 bg-neutral-900/50 overflow-y-auto p-4",
                    ComparisonPanel { selected }
                }
            }
            // Footer: unit database version + upstream attribution.
            if let Some(Ok(meta)) = meta.read().as_ref() {
                div { class: "shrink-0 border-t border-neutral-800 bg-neutral-900/60 px-4 py-1.5 text-xs text-neutral-400 flex items-center gap-1.5",
                    span { "Data version: {meta.version} · {meta.unit_count} units" }
                    span { class: "text-neutral-600", "|" }
                    span { "Source: " }
                    a {
                        class: "text-sky-400 hover:text-sky-300 underline underline-offset-2",
                        href: "{meta.source_url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "{meta.source_name}"
                    }
                }
            }
        }
    }
}
