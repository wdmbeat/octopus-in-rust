use dioxus::prelude::*;

use crate::components::UnitSummary;
use crate::utils::faction_glow_class;

/// Unit id → strategic-icon overlay URL (`None` value = explicitly no icon).
/// When the map itself is `None`, the blueprint default static PNG is used.
pub type IconOverrides = std::collections::HashMap<String, Option<String>>;

/// Compact square portrait with faction glow and optional strategic icon overlay.
#[component]
pub fn UnitIcon(
    unit: UnitSummary,
    faction: String,
    selected: bool,
    on_select: EventHandler<UnitSummary>,
    /// Effective-icon overrides (Icons page): replaces the blueprint default
    /// overlay with the icon the unit would show under the enabled mods.
    #[props(default)]
    icon_overrides: Option<IconOverrides>,
    /// Tailwind size classes for the strategic-icon overlay (Icons page
    /// shows them bigger); defaults to `"w-3.5 h-3.5"`.
    #[props(default)]
    overlay_class: Option<&'static str>,
) -> Element {
    let id = unit.id.clone();
    let name = unit.name.clone();
    let glow = faction_glow_class(&faction);
    let portrait_src = crate::net::portrait_url(&id);
    let strategic_src = match &icon_overrides {
        Some(map) => map.get(&id.to_ascii_uppercase()).cloned().flatten(),
        None => unit
            .strategic_icon_name
            .as_deref()
            .map(|icon| format!("/strategic/{}_{}.png", faction, icon)),
    };

    rsx! {
        button {
            class: "relative w-12 h-12 p-[3px] rounded-[5px] bg-black border cursor-pointer transition-transform hover:scale-105 active:scale-[0.99] active:translate-y-px {glow}",
            class: if selected { "ring-2 ring-white" },
            title: "{name}",
            onclick: move |_| on_select.call(unit.clone()),
            img {
                src: "{portrait_src}",
                alt: "{name}",
                class: "w-full h-full object-contain block",
            }
            if let Some(src) = strategic_src {
                img {
                    src: "{src}",
                    alt: "",
                    class: "absolute top-0.5 left-0.5 {overlay_class.unwrap_or(\"w-3.5 h-3.5\")} object-contain pointer-events-none",
                }
            }
        }
    }
}
