use dioxus::prelude::*;
use faf_blueprints::TechLevel;
use std::collections::HashSet;

use crate::components::unit_icon::IconOverrides;
use crate::components::{UnitIcon, UnitSummary};
use crate::utils::{tech_level_short, CATEGORY_ORDER, FACTION_ORDER};

/// Reusable unit picker with search, faction/kind/tech filters, and category grid.
///
/// `selected` is an optional display-only set of highlighted unit ids; the
/// picker itself stays single-select (callers decide what a click means).
/// `icon_overrides` (Icons page) replaces each unit's blueprint strategic
/// overlay with its effective icon under the enabled mods.
#[component]
pub fn UnitSelector(
    units: Vec<UnitSummary>,
    on_select: EventHandler<UnitSummary>,
    #[props(default)] selected: HashSet<String>,
    #[props(default)] icon_overrides: Option<IconOverrides>,
    #[props(default)] overlay_class: Option<&'static str>,
) -> Element {
    let mut query = use_signal(String::new);
    let active_factions = use_signal(HashSet::<String>::new);
    let active_kinds = use_signal(HashSet::<String>::new);
    let active_techs = use_signal(HashSet::<String>::new);

    let filtered: Vec<UnitSummary> = units
        .into_iter()
        .filter(|u| {
            let q = query.read().to_lowercase();
            let text_match = q.is_empty()
                || u.id.to_lowercase().contains(&q)
                || u.name.to_lowercase().contains(&q)
                || u.description.to_lowercase().contains(&q);
            let faction_match = active_factions.read().is_empty()
                || active_factions.read().contains(&u.faction.to_lowercase());
            let kind_match = active_kinds.read().is_empty()
                || u.kind
                    .as_ref()
                    .map(|k| active_kinds.read().contains(k))
                    .unwrap_or(false);
            let tech_match = active_techs.read().is_empty()
                || active_techs.read().contains(tech_level_short(u.tech_level));
            text_match && faction_match && kind_match && tech_match
        })
        .collect();

    rsx! {
        div { class: "flex flex-col flex-1 overflow-hidden",
            header { class: "flex flex-wrap items-center gap-4 px-4 py-3 border-b border-neutral-800 bg-neutral-900/50 shrink-0",
                input {
                    class: "flex-1 min-w-[12rem] max-w-sm px-3 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-white placeholder-neutral-500 focus:outline-none focus:border-blue-500",
                    r#type: "text",
                    placeholder: "Search units...",
                    value: "{query.read()}",
                    oninput: move |e| query.set(e.value()),
                }
                FilterBar {
                    active_factions,
                    active_kinds,
                    active_techs,
                }
            }
            div { class: "flex-1 overflow-auto p-4",
                CategoryGrid { units: filtered, on_select, selected, icon_overrides, overlay_class }
            }
        }
    }
}

#[component]
fn FilterBar(
    active_factions: Signal<HashSet<String>>,
    active_kinds: Signal<HashSet<String>>,
    active_techs: Signal<HashSet<String>>,
) -> Element {
    rsx! {
        div { class: "flex items-center gap-4",
            FilterGroup {
                items: vec!["uef", "cybran", "aeon", "seraphim"],
                active: active_factions,
                icon_dir: "embed_icons",
                extension: "svg",
            }
            FilterGroup {
                items: vec!["Base", "Land", "Air", "Naval"],
                active: active_kinds,
                icon_dir: "ui",
                extension: "png",
            }
            FilterGroup {
                items: vec!["T1", "T2", "T3", "EXP"],
                active: active_techs,
                icon_dir: "ui",
                extension: "png",
            }
        }
    }
}

#[component]
fn FilterGroup(
    items: Vec<&'static str>,
    active: Signal<HashSet<String>>,
    icon_dir: &'static str,
    extension: &'static str,
) -> Element {
    rsx! {
        div { class: "flex items-center gap-1",
            for item in items {
                FilterButton {
                    key: "{item}",
                    item,
                    active,
                    icon_dir,
                    extension,
                }
            }
        }
    }
}

#[component]
fn FilterButton(
    item: &'static str,
    active: Signal<HashSet<String>>,
    icon_dir: &'static str,
    extension: &'static str,
) -> Element {
    let is_active = active.read().contains(item);
    let active_class = if is_active {
        "opacity-100 bg-white/15 ring-1 ring-white/30"
    } else {
        "opacity-75 hover:opacity-100 bg-neutral-800/50 hover:bg-neutral-700/50"
    };
    let src = format!("/{}/{}.{}?v=1", icon_dir, item, extension);

    rsx! {
        button {
            class: "w-8 h-8 p-1 rounded cursor-pointer transition-all {active_class}",
            title: "{item}",
            onclick: move |_| {
                active.with_mut(|set| {
                    if !set.remove(item) {
                        set.insert(item.to_string());
                    }
                });
            },
            img { src: "{src}", alt: "{item}", class: "w-full h-full object-contain" }
        }
    }
}

#[component]
fn CategoryGrid(
    units: Vec<UnitSummary>,
    on_select: EventHandler<UnitSummary>,
    #[props(default)] selected: HashSet<String>,
    #[props(default)] icon_overrides: Option<IconOverrides>,
    #[props(default)] overlay_class: Option<&'static str>,
) -> Element {
    let mut by_category: std::collections::HashMap<String, Vec<UnitSummary>> =
        std::collections::HashMap::new();
    for unit in units {
        if let Some(category) = unit.category.clone() {
            by_category.entry(category).or_default().push(unit);
        }
    }

    let mut ordered: Vec<(String, Vec<UnitSummary>)> = Vec::new();
    for category in CATEGORY_ORDER.iter().copied() {
        if let Some(group) = by_category.remove(category) {
            ordered.push((category.to_string(), group));
        }
    }

    rsx! {
        div { class: "flex flex-wrap gap-4 items-start content-start",
            if ordered.is_empty() {
                div { class: "text-neutral-500 text-sm text-center py-8 w-full",
                    "No units match the current filters."
                }
            }
            for (category, units) in ordered {
                CategoryPanel {
                    key: "{category}",
                    category,
                    units,
                    on_select,
                    selected: selected.clone(),
                    icon_overrides: icon_overrides.clone(),
                    overlay_class,
                }
            }
        }
    }
}

#[component]
fn CategoryPanel(
    category: String,
    units: Vec<UnitSummary>,
    on_select: EventHandler<UnitSummary>,
    #[props(default)] selected: HashSet<String>,
    #[props(default)] icon_overrides: Option<IconOverrides>,
    #[props(default)] overlay_class: Option<&'static str>,
) -> Element {
    if units.is_empty() {
        return rsx! {};
    }

    let techs: Vec<TechLevel> = [TechLevel::T1, TechLevel::T2, TechLevel::T3, TechLevel::T4]
        .into_iter()
        .filter(|tech| units.iter().any(|u| u.tech_level == *tech))
        .collect();

    rsx! {
        div { class: "border border-neutral-700 rounded-lg bg-neutral-900/80 backdrop-blur-sm p-3 shadow-lg",
            h2 { class: "text-sm font-semibold text-center text-white mb-3 tracking-wide", "{category}" }
            div { class: "flex",
                for (i, tech) in techs.iter().enumerate() {
                    div {
                        key: "{tech:?}",
                        class: "flex flex-col gap-1.5",
                        class: if i > 0 { "pl-1.5" },
                        class: if i < techs.len() - 1 { "pr-1.5 border-r-2 border-dashed border-white/60" },
                        h4 { class: "text-[10px] uppercase tracking-wider text-neutral-500 text-center", "{tech:?}" }
                        for faction in FACTION_ORDER.iter().copied() {
                            TechCell {
                                units: units.iter().filter(|u| u.faction.to_lowercase() == faction.to_lowercase() && u.tech_level == *tech).cloned().collect::<Vec<_>>(),
                                faction,
                                on_select,
                                selected: selected.clone(),
                                icon_overrides: icon_overrides.clone(),
                                overlay_class,
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TechCell(
    units: Vec<UnitSummary>,
    faction: &'static str,
    on_select: EventHandler<UnitSummary>,
    #[props(default)] selected: HashSet<String>,
    #[props(default)] icon_overrides: Option<IconOverrides>,
    #[props(default)] overlay_class: Option<&'static str>,
) -> Element {
    if units.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "flex flex-wrap gap-1.5",
            for unit in units {
                UnitIcon {
                    key: "{unit.id}",
                    unit: unit.clone(),
                    faction: faction.to_string(),
                    selected: selected.contains(&unit.id),
                    on_select,
                    icon_overrides: icon_overrides.clone(),
                    overlay_class,
                }
            }
        }
    }
}
