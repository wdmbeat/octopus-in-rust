//! Icons page: the central place to align units with strategic icons.
//!
//! Icon sets are FAF mods (ReduxStrategicIconsLarge, Calibersexp,
//! SACUIcons, …) toggled like in game; the unit grid previews the effective
//! in-game icon per unit and highlights uncovered ones. The icon-class
//! picker (moved here from the datagen page) chooses which classes synthetic
//! data generation trains on. Both are persisted in `icon-config.json`.

use std::collections::HashSet;

use dioxus::prelude::*;
use faf_ml_core::{IconClassInfo, IconConfig, IconSetInfo, UnitIconEffective};
use gloo_net::http::Request;

use crate::components::unit_icon::IconOverrides;
use crate::components::{UnitSelector, UnitSummary};
use crate::Route;

/// `GET /api/icons/sets` — registered icon-set mods with enabled flags.
async fn fetch_sets() -> Result<Vec<IconSetInfo>, String> {
    Request::get(&crate::net::api_url("/api/icons/sets"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<IconSetInfo>>()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/icons/config` — the saved configuration.
async fn fetch_config() -> Result<IconConfig, String> {
    Request::get(&crate::net::api_url("/api/icons/config"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<IconConfig>()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/icons/units[?mods=...]` — effective icon per unit.
async fn fetch_effective(mods: Option<String>) -> Result<Vec<UnitIconEffective>, String> {
    let path = match mods {
        Some(mods) => format!("/api/icons/units?mods={mods}"),
        None => "/api/icons/units".to_string(),
    };
    Request::get(&crate::net::api_url(&path))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<UnitIconEffective>>()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/icons/classes[?mods=...]` — per-class source/coverage info.
async fn fetch_classes(mods: Option<String>) -> Result<Vec<IconClassInfo>, String> {
    let path = match mods {
        Some(mods) => format!("/api/icons/classes?mods={mods}"),
        None => "/api/icons/classes".to_string(),
    };
    Request::get(&crate::net::api_url(&path))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<IconClassInfo>>()
        .await
        .map_err(|e| e.to_string())
}

/// `/api/units` — unit summaries for the preview grid (same source as the
/// Units page browser).
async fn fetch_units() -> Result<Vec<UnitSummary>, String> {
    Request::get(&crate::net::api_url("/api/units"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<UnitSummary>>()
        .await
        .map_err(|e| e.to_string())
}

/// `PUT /api/icons/config` — persist the selection; surfaces server errors.
async fn save_config(config: &IconConfig) -> Result<(), String> {
    let resp = Request::put(&crate::net::api_url("/api/icons/config"))
        .json(config)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.ok() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(format!("HTTP {status}: {text}"))
    }
}

/// Comma-joined enabled mod ids for the `?mods=` preview query.
fn mods_query(enabled: &HashSet<String>) -> String {
    let mut ids: Vec<&str> = enabled.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.join(",")
}

/// Icons page (workflow step 2): mod toggles + unit-icon preview + the
/// icon-class picker that feeds synthetic data generation.
#[component]
pub fn Icons() -> Element {
    let navigator = use_navigator();
    let mut status = use_signal(String::new);
    // Local working copy of the config; initialized from the server, edited
    // by the toggles, persisted by Save.
    let mut enabled: Signal<HashSet<String>> = use_signal(HashSet::new);
    let mut excluded: Signal<HashSet<String>> = use_signal(HashSet::new);
    // Bumped by Save so the sets resource refetches fresh enabled flags.
    let mut refresh = use_signal(|| 0u32);

    let sets = use_resource(move || async move {
        refresh();
        fetch_sets().await
    });
    // Config load + one-time initialization of the working copy.
    let config = use_resource(fetch_config);
    let mut initialized = use_signal(|| false);
    use_effect(move || {
        if *initialized.read() {
            return;
        }
        if let Some(Ok(saved)) = config.read().as_ref() {
            enabled.set(saved.enabled_mods.iter().cloned().collect());
            excluded.set(saved.excluded_classes.iter().cloned().collect());
            initialized.set(true);
        }
    });

    let units = use_resource(fetch_units);

    // Live previews, re-run whenever the working mod selection changes.
    let effective = use_resource(move || {
        let mods = mods_query(&enabled.read());
        async move { fetch_effective(Some(mods)).await }
    });
    let classes = use_resource(move || {
        let mods = mods_query(&enabled.read());
        async move { fetch_classes(Some(mods)).await }
    });

    // unit id → effective overlay URL (None = uncovered: no overlay).
    let icon_overrides: Option<IconOverrides> = effective.read().as_ref().and_then(|r| {
        r.as_ref().ok().map(|list| {
            list.iter()
                .map(|e| {
                    (
                        e.unit_id.clone(),
                        e.class
                            .as_ref()
                            .map(|c| crate::net::api_url(&format!("/api/icons/sprites/{c}/image"))),
                    )
                })
                .collect()
        })
    });
    let uncovered: HashSet<String> = effective
        .read()
        .as_ref()
        .and_then(|r| r.as_ref().ok().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.class.is_none())
        .map(|e| e.unit_id)
        .collect();

    rsx! {
        div { class: "flex-1 min-h-0 flex flex-col bg-neutral-950 text-gray-200 font-sans",
            div { class: "shrink-0 px-6 pt-6 max-w-none",
                crate::workflow::WorkflowBanner { step: 1 }
            }
            div { class: "flex-1 min-h-0 flex overflow-hidden",
                // Left column: configuration cards.
                div { class: "w-[26rem] shrink-0 overflow-y-auto border-r border-neutral-800 p-4 space-y-4",
                    h1 { class: "text-2xl font-bold text-white", "Unit icons" }
                    p { class: "text-xs text-neutral-400",
                        "Strategic icons are configurable like in-game mods: enable icon sets "
                        "below and the preview on the right shows the icon each unit would have "
                        "in game. Excluded classes are left out of synthetic data generation."
                    }

                    // ── Icon sets ────────────────────────────────────────────
                    div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-4",
                        h2 { class: "text-sm font-semibold text-white mb-3", "Icon sets" }
                        match &*sets.read() {
                            None => rsx! { p { class: "text-xs text-neutral-500", "Loading icon sets..." } },
                            Some(Err(e)) => rsx! { p { class: "text-xs text-red-400", "{e}" } },
                            Some(Ok(list)) => rsx! {
                                div { class: "space-y-2",
                                    for set in list.iter() {
                                        {
                                            let id = set.id.clone();
                                            let checked = set.builtin || enabled.read().contains(&set.id);
                                            rsx! {
                                                label {
                                                    key: "{set.id}",
                                                    class: "flex items-start gap-2 text-sm text-neutral-200 cursor-pointer",
                                                    input {
                                                        r#type: "checkbox",
                                                        class: "mt-1 accent-blue-500",
                                                        checked,
                                                        disabled: set.builtin,
                                                        onchange: move |_| {
                                                            let mut set = enabled.write();
                                                            if !set.remove(&id) {
                                                                set.insert(id.clone());
                                                            }
                                                        },
                                                    }
                                                    div {
                                                        div { class: "font-medium",
                                                            "{set.name} "
                                                            if let Some(v) = set.version {
                                                                span { class: "text-xs text-neutral-500", "v{v}" }
                                                            }
                                                            if set.builtin {
                                                                span { class: "text-xs text-neutral-500", " (fallback)" }
                                                            }
                                                        }
                                                        div { class: "text-[11px] text-neutral-500",
                                                            "{set.class_count} classes · {set.assignment_count} explicit assignments · {set.id}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── Icon classes ─────────────────────────────────────────
                    div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-4",
                        match &*classes.read() {
                            None => rsx! { p { class: "text-xs text-neutral-500", "Loading sprite classes..." } },
                            Some(Err(e)) => rsx! { p { class: "text-xs text-red-400", "{e}" } },
                            Some(Ok(list)) => {
                                let total = list.len();
                                let excluded_count = excluded.read().len();
                                // Excluded classes that still cover units.
                                let hidden: Vec<&IconClassInfo> = list
                                    .iter()
                                    .filter(|c| excluded.read().contains(&c.class) && c.unit_count > 0)
                                    .collect();
                                rsx! {
                                    div { class: "flex items-center gap-2 mb-2",
                                        span { class: "text-sm font-semibold text-white",
                                            "Icon classes — {total - excluded_count} / {total} included"
                                        }
                                        div { class: "flex-1" }
                                        button {
                                            class: "px-2 py-0.5 rounded text-[11px] text-neutral-300 bg-neutral-800 hover:bg-neutral-700",
                                            onclick: move |_| excluded.write().clear(),
                                            "include all"
                                        }
                                        button {
                                            class: "px-2 py-0.5 rounded text-[11px] text-neutral-300 bg-neutral-800 hover:bg-neutral-700",
                                            onclick: {
                                                let all: HashSet<String> =
                                                    list.iter().map(|c| c.class.clone()).collect();
                                                move |_| *excluded.write() = all.clone()
                                            },
                                            "exclude all"
                                        }
                                    }
                                    p { class: "text-[11px] text-neutral-500 mb-2",
                                        "Click an icon to exclude/include it (hover for the class name). Dimmed icons are left out of the synthetic data."
                                    }
                                    if !hidden.is_empty() {
                                        div { class: "text-[11px] text-amber-400 mb-2 space-y-0.5",
                                            for c in hidden {
                                                p { key: "warn-{c.class}",
                                                    "⚠ {c.class} excluded — {c.unit_count} unit(s) lose their only icon"
                                                }
                                            }
                                        }
                                    }
                                    div { class: "max-h-72 overflow-y-auto grid grid-cols-6 gap-1 pr-1 rounded border border-neutral-800 bg-neutral-950 p-2",
                                        for info in list.iter() {
                                            {
                                                let n = info.class.clone();
                                                let included = !excluded.read().contains(&info.class);
                                                let cell_class = if included {
                                                    "relative p-1 rounded border border-blue-500/60 bg-neutral-800 hover:border-blue-400 cursor-pointer transition-all"
                                                } else {
                                                    "relative p-1 rounded border border-transparent opacity-30 grayscale hover:opacity-70 cursor-pointer transition-all"
                                                };
                                                let source = info.source.clone();
                                                rsx! {
                                                    button {
                                                        key: "{info.class}",
                                                        class: cell_class,
                                                        title: "{info.class} ({source}, {info.unit_count} units)",
                                                        onclick: move |_| {
                                                            let mut set = excluded.write();
                                                            if !set.remove(&n) {
                                                                set.insert(n.clone());
                                                            }
                                                        },
                                                        img {
                                                            class: "w-9 h-10 block mx-auto",
                                                            src: crate::net::api_url(&format!(
                                                                "/api/icons/sprites/{}/image", info.class
                                                            )),
                                                            alt: "{info.class}",
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── Save ─────────────────────────────────────────────────
                    button {
                        class: "px-4 py-2 rounded bg-blue-700 hover:bg-blue-600 text-white text-sm font-semibold transition-colors",
                        onclick: move |_| {
                            let config = IconConfig {
                                enabled_mods: enabled.read().iter().cloned().collect(),
                                excluded_classes: excluded.read().iter().cloned().collect(),
                            };
                            spawn(async move {
                                match save_config(&config).await {
                                    Ok(()) => {
                                        status.set("saved — datagen will use this configuration".to_string());
                                        *refresh.write() += 1;
                                    }
                                    Err(e) => status.set(e),
                                }
                            });
                        },
                        "Save configuration"
                    }
                    if !status.read().is_empty() {
                        p { class: "text-xs text-amber-400", "{status}" }
                    }
                }

                // Right column: unit preview grid.
                div { class: "flex-1 min-h-0 flex flex-col",
                    div { class: "shrink-0 px-4 py-2 border-b border-neutral-800 bg-neutral-900/50 text-xs flex items-center gap-2",
                        if uncovered.is_empty() {
                            span { class: "text-green-400", "every unit has an icon under this selection" }
                        } else {
                            span { class: "text-amber-400",
                                "{uncovered.len()} units uncovered (highlighted) — enable more icon sets or include their class"
                            }
                        }
                    }
                    match &*units.read() {
                        None => rsx! {
                            div { class: "flex items-center justify-center h-full text-neutral-400", "Loading..." }
                        },
                        Some(Err(e)) => rsx! {
                            div { class: "flex items-center justify-center h-full text-red-400", "Failed to load units: {e}" }
                        },
                        Some(Ok(unit_list)) => rsx! {
                            UnitSelector {
                                units: unit_list.clone(),
                                selected: uncovered,
                                icon_overrides,
                                overlay_class: "w-10 h-10",
                                on_select: move |unit: UnitSummary| {
                                    navigator.push(Route::UnitDetail { id: unit.id.clone() });
                                },
                            }
                        },
                    }
                }
            }
        }
    }
}
