use dioxus::prelude::*;

use crate::i18n::{self, Text};
use crate::Route;

/// Hero background (screenshot courtesy of faforever.com).
const HERO: Asset = asset!("/assets/home-hero.webp");

/// The community QQ group number.
const QQ_GROUP: &str = "136430130";

/// Landing page: faforever.com-style hero + feature cards + QQ group section.
#[component]
pub fn Home() -> Element {
    let t = i18n::use_t();
    let mut copied = use_signal(|| false);

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans",
            // Hero: full-bleed game screenshot with headline + CTAs.
            div {
                class: "relative flex items-center justify-center min-h-[70vh]",
                style: "background-image: linear-gradient(to bottom, rgba(10,10,10,0.55), rgba(10,10,10,0.88)), url({HERO}); background-size: cover; background-position: center;",
                div { class: "text-center px-6 py-24 max-w-3xl",
                    p { class: "text-sm tracking-[0.3em] text-amber-400 font-semibold mb-4",
                        "{t.t(Text::HomeHeroKicker)}"
                    }
                    h1 { class: "text-4xl md:text-5xl font-bold text-white mb-4",
                        "{t.t(Text::HomeHeroTitle)}"
                    }
                    p { class: "text-lg text-neutral-300 mb-8",
                        "{t.t(Text::HomeHeroSubtitle)}"
                    }
                    div { class: "flex flex-wrap items-center justify-center gap-4",
                        Link {
                            class: "px-6 py-3 rounded bg-blue-700 hover:bg-blue-600 text-white font-semibold transition-colors",
                            to: Route::Guide {},
                            "{t.t(Text::HomeCtaGuide)}"
                        }
                        Link {
                            class: "px-6 py-3 rounded bg-neutral-700 hover:bg-neutral-600 text-white font-semibold transition-colors",
                            to: Route::Sync {},
                            "{t.t(Text::HomeCtaSync)}"
                        }
                        button {
                            class: "px-6 py-3 rounded bg-amber-500 hover:bg-amber-400 text-black font-semibold transition-colors",
                            onclick: move |_| {
                                copy_to_clipboard(QQ_GROUP);
                                copied.set(true);
                            },
                            if *copied.read() {
                                "{t.t(Text::HomeQQCopied)} {QQ_GROUP}"
                            } else {
                                "{t.t(Text::HomeCtaQQ)}: {QQ_GROUP}"
                            }
                        }
                    }
                }
            }

            // Community-run disclaimer, prominent on the landing page
            // (requested by FAF staff: faforever.cn must not be mistaken for
            // an official FAF service).
            div { class: "max-w-5xl mx-auto px-6 pt-8",
                div { class: "rounded-lg border border-blue-500/30 bg-blue-500/10 px-5 py-4 text-sm text-blue-200 text-center",
                    "{t.t(Text::HomeCommunityNote)} "
                    a {
                        class: "text-blue-300 hover:text-blue-200 underline underline-offset-2 font-semibold",
                        href: "https://faforever.com",
                        target: "_blank",
                        rel: "noopener",
                        "faforever.com"
                    }
                }
            }

            // Why Supreme Commander: detail-driven vignettes, not marketing.
            div { class: "max-w-5xl mx-auto px-6 pt-14",
                h2 { class: "text-2xl font-bold text-white mb-6 text-center",
                    "{t.t(Text::HomeWhyTitle)}"
                }
                div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
                    Vignette {
                        title: t.t(Text::VigReclaimTitle),
                        body: t.t(Text::VigReclaimBody),
                        punch: t.t(Text::VigReclaimPunch),
                    }
                    Vignette {
                        title: t.t(Text::VigZoomTitle),
                        body: t.t(Text::VigZoomBody),
                        punch: t.t(Text::VigZoomPunch),
                    }
                    Vignette {
                        title: t.t(Text::VigPhysicsTitle),
                        body: t.t(Text::VigPhysicsBody),
                        punch: t.t(Text::VigPhysicsPunch),
                    }
                    Vignette {
                        title: t.t(Text::VigExpTitle),
                        body: t.t(Text::VigExpBody),
                        punch: t.t(Text::VigExpPunch),
                    }
                    Vignette {
                        title: t.t(Text::VigAcuTitle),
                        body: t.t(Text::VigAcuBody),
                        punch: t.t(Text::VigAcuPunch),
                    }
                    Vignette {
                        title: t.t(Text::VigCommunityTitle),
                        body: t.t(Text::VigCommunityBody),
                        punch: t.t(Text::VigCommunityPunch),
                    }
                }
                p { class: "text-xs text-neutral-500 mt-6 text-center", "{t.t(Text::WhyPriceNote)}" }
            }

            // Feature cards linking to the app's pages.
            div { class: "max-w-5xl mx-auto px-6 py-16",
                h2 { class: "text-2xl font-bold text-white mb-8 text-center",
                    "{t.t(Text::HomeFeaturesTitle)}"
                }
                div { class: "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4",
                    FeatureCard {
                        to: Route::Units {},
                        title: t.t(Text::FeatureUnitsTitle),
                        desc: t.t(Text::FeatureUnitsDesc),
                    }
                    FeatureCard {
                        to: Route::Simulate {},
                        title: t.t(Text::FeatureSimTitle),
                        desc: t.t(Text::FeatureSimDesc),
                    }
                    FeatureCard {
                        to: Route::Qa {},
                        title: t.t(Text::FeatureQaTitle),
                        desc: t.t(Text::FeatureQaDesc),
                    }
                    FeatureCard {
                        to: Route::Sync {},
                        title: t.t(Text::FeatureSyncTitle),
                        desc: t.t(Text::FeatureSyncDesc),
                    }
                }
            }

            // QQ group section.
            div { class: "max-w-5xl mx-auto px-6 pb-16",
                div { class: "rounded-xl border border-amber-500/30 bg-gradient-to-r from-amber-500/10 to-transparent p-8 text-center",
                    h2 { class: "text-2xl font-bold text-white mb-2", "{t.t(Text::HomeQQTitle)}" }
                    p { class: "text-neutral-400 mb-4", "{t.t(Text::HomeQQDesc)}" }
                    div { class: "text-4xl font-mono font-bold text-amber-400 tracking-[0.3em] mb-4 select-all",
                        "{QQ_GROUP}"
                    }
                    button {
                        class: "px-5 py-2 rounded bg-amber-500 hover:bg-amber-400 text-black text-sm font-semibold transition-colors",
                        onclick: move |_| {
                            copy_to_clipboard(QQ_GROUP);
                            copied.set(true);
                        },
                        if *copied.read() {
                            "{t.t(Text::HomeQQCopied)}"
                        } else {
                            "{t.t(Text::HomeCtaQQ)}"
                        }
                    }
                }
            }
        }
    }
}

/// One why-SupCom vignette: bold title, detailed body, insider punchline.
#[component]
fn Vignette(title: &'static str, body: &'static str, punch: &'static str) -> Element {
    rsx! {
        div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-5",
            h3 { class: "text-base font-semibold text-amber-300 mb-2", "{title}" }
            p { class: "text-sm text-neutral-300 leading-relaxed mb-3", "{body}" }
            p { class: "text-xs text-neutral-500 italic", "{punch}" }
        }
    }
}

/// One feature card linking to an app page.
#[component]
fn FeatureCard(to: Route, title: String, desc: String) -> Element {
    rsx! {
        Link {
            to,
            class: "block rounded-lg border border-neutral-800 bg-neutral-900 p-5 hover:border-blue-500 hover:bg-neutral-800/60 transition-colors",
            h3 { class: "text-lg font-semibold text-white mb-2", "{title}" }
            p { class: "text-sm text-neutral-400", "{desc}" }
        }
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}
