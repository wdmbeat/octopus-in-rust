use dioxus::prelude::*;

use crate::components::UnitBrowser;

/// Units page: the unit database browser (ported from fafcn-web's unit
/// browser; will later host the strategic-icon ↔ unit mapping work).
#[component]
pub fn Units() -> Element {
    rsx! {
        div { class: "flex-1 min-h-0 flex flex-col",
            UnitBrowser {}
        }
    }
}
