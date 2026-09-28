//! Web frontend for the FAF construction-plan simulator.
//!
//! Connects to `fafcn-server` via WebSocket to run simulations and renders
//! the streamed economy events with components from `faf-dioxus-ui`.

// rsx! formatted strings ("{var}") are the idiomatic Dioxus style, but the
// macro expands them to format!() and trips this lint — allow it crate-wide.
#![allow(clippy::useless_format)]

use dioxus::prelude::*;

mod components;
mod i18n;
mod net;
mod state;
mod utils;
mod views;

use views::{Guide, Home, Navbar, Qa, Simulate, Sync, Units};

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(Navbar)]
        #[route("/")]
        Home {},
        #[route("/units")]
        Units {},
        #[route("/simulate")]
        Simulate {},
        #[route("/qa")]
        Qa {},
        #[route("/sync")]
        Sync {},
        #[route("/onboarding")]
        Guide {},
}

const FAVICON: Asset = asset!("/assets/favicon.ico");
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    i18n::use_provide_lang();
    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        Router::<Route> {}
    }
}
