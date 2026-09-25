use dioxus::prelude::*;
use fafcn_gamedata::Manifest;
use gloo_net::http::Request;

use crate::i18n::{self, Text};
use crate::Route;

/// Steam store page for Supreme Commander: Forged Alliance (the base game
/// every FAF player must own; FAF verifies ownership via Steam account
/// linking).
const STEAM_STORE: &str =
    "https://store.steampowered.com/app/9420/Supreme_Commander_Forged_Alliance/";

/// Official FAF account registration page.
const FAF_REGISTER: &str = "https://www.faforever.com/account/register";

/// GitHub releases page for the official FAF client.
const GITHUB_RELEASES: &str = "https://github.com/FAForever/downlords-faf-client/releases";

/// QiYou accelerator official site (recommended for FAF from China).
const QIYOU: &str = "https://www.qiyou.cn";

/// Bilibili video tutorial: FAF account registration (B 站).
const REGISTER_VIDEO: &str = "https://www.bilibili.com/video/BV1aqkMBEEDw/";

/// Onboarding guide: five steps from zero to the first online game.
#[component]
pub fn Guide() -> Element {
    let t = i18n::use_t();
    // The faf-client manifest decides step 2: mirror download vs GitHub link.
    let faf_client = use_resource(move || async move {
        let resp = Request::get(&crate::net::api_url(
            "/api/gamedata/channels/faf-client/manifest.json",
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
        if resp.status() == 404 {
            return Ok(None);
        }
        resp.json::<Manifest>()
            .await
            .map(Some)
            .map_err(|e| e.to_string())
    });

    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans",
            div { class: "max-w-2xl mx-auto px-6 py-8 flex flex-col gap-6",
                h1 { class: "text-2xl font-bold text-white", "{t.t(Text::GuideTitle)}" }
                p { class: "text-sm text-neutral-400", "{t.t(Text::GuideIntro)}" }

                // Step 1: buy & install the base game on Steam.
                GuideStepCard {
                    number: "1",
                    title: t.t(Text::GuideStep1Title),
                    p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep1Desc)}" }
                    div { class: "flex flex-wrap gap-3 mt-3",
                        GuideLinkButton { href: STEAM_STORE, label: t.t(Text::GuideStep1SteamBtn) }
                    }
                    p { class: "text-xs text-neutral-500 mt-3", "{t.t(Text::GuideStep1Note)}" }
                }

                // Step 2: download the FAF client.
                GuideStepCard {
                    number: "2",
                    title: t.t(Text::GuideStep2Title),
                    match faf_client.read().as_ref() {
                        Some(Ok(Some(manifest))) => rsx! {
                            p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep2Mirror)}" }
                            div { class: "flex flex-wrap gap-3 mt-2",
                                for file in &manifest.files {
                                    a {
                                        class: "inline-block px-4 py-2 rounded bg-emerald-700 hover:bg-emerald-600 text-white text-sm transition-colors",
                                        href: crate::net::api_url(&format!("/api/gamedata/channels/faf-client/files/{}", file.path)),
                                        "{t.t(Text::DownloadFafClient)} ({file.path})"
                                    }
                                }
                            }
                        },
                        _ => rsx! {
                            p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep2Github)}" }
                            div { class: "flex flex-wrap gap-3 mt-3",
                                GuideLinkButton { href: GITHUB_RELEASES, label: t.t(Text::GuideStep2GithubBtn) }
                            }
                        },
                    }
                    p { class: "text-xs text-neutral-500 mt-3", "{t.t(Text::GuideStep2Note)}" }
                }

                // Step 3: register a FAF account (Outlook email recommended).
                GuideStepCard {
                    number: "3",
                    title: t.t(Text::GuideStep3Title),
                    p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep3Desc)}" }
                    div { class: "flex flex-wrap gap-3 mt-3",
                        GuideLinkButton { href: FAF_REGISTER, label: t.t(Text::GuideStep3RegisterBtn) }
                    }
                    p { class: "text-xs text-neutral-500 mt-3", "{t.t(Text::GuideStep3Link)}" }
                    a {
                        class: "inline-flex items-center gap-1 mt-2 text-sm text-blue-400 hover:text-blue-300 underline underline-offset-2",
                        href: REGISTER_VIDEO,
                        target: "_blank",
                        "▶ {t.t(Text::GuideStep3Video)}"
                    }
                }

                // Step 4: download our sync client for patches & maps.
                GuideStepCard {
                    number: "4",
                    title: t.t(Text::GuideStep4Title),
                    p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep4Desc)}" }
                    div { class: "flex flex-wrap gap-3 mt-3",
                        Link {
                            class: "inline-block px-4 py-2 rounded bg-blue-700 hover:bg-blue-600 text-white text-sm transition-colors",
                            to: Route::Sync {},
                            "{t.t(Text::GuideStep4Btn)}"
                        }
                    }
                }

                // Step 5: accelerate with QiYou, then play.
                GuideStepCard {
                    number: "5",
                    title: t.t(Text::GuideStep5Title),
                    p { class: "text-sm text-neutral-300", "{t.t(Text::GuideStep5Desc)}" }
                    div { class: "flex flex-wrap gap-3 mt-3",
                        GuideLinkButton { href: QIYOU, label: t.t(Text::GuideStep5Btn) }
                    }
                    p { class: "text-xs text-neutral-500 mt-3", "{t.t(Text::GuideStep5Note)}" }
                }
            }
        }
    }
}

/// One numbered step card.
#[component]
fn GuideStepCard(number: String, title: String, children: Element) -> Element {
    rsx! {
        div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-5",
            div { class: "flex items-center gap-3 mb-3",
                span { class: "flex items-center justify-center w-8 h-8 rounded-full bg-blue-700 text-white font-bold text-sm shrink-0",
                    "{number}"
                }
                h2 { class: "text-lg font-semibold text-white", "{title}" }
            }
            {children}
        }
    }
}

/// External-link button shared by the guide steps (opens in a new tab).
#[component]
fn GuideLinkButton(href: &'static str, label: String) -> Element {
    rsx! {
        a {
            class: "inline-block px-4 py-2 rounded bg-neutral-700 hover:bg-neutral-600 text-white text-sm transition-colors",
            href,
            target: "_blank",
            rel: "noopener",
            "{label} ↗"
        }
    }
}
