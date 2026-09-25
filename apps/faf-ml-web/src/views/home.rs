use dioxus::prelude::*;

use crate::workflow;

/// Landing page: what this platform is + the workflow checklist.
#[component]
pub fn Home() -> Element {
    rsx! {
        div { class: "flex-1 overflow-y-auto bg-neutral-950 text-gray-200 font-sans",
            div { class: "max-w-3xl mx-auto px-6 py-16",
                p { class: "text-sm tracking-[0.3em] text-amber-400 font-semibold mb-4",
                    "FAF MACHINE LEARNING"
                }
                h1 { class: "text-4xl font-bold text-white mb-4", "faf-ml data platform" }
                p { class: "text-lg text-neutral-300 mb-10",
                    "The FAF unit-detection pipeline: collect screenshots, generate and "
                    "review labeled data, then train and monitor a detector that "
                    "identifies units from a screenshot."
                }
                div { class: "rounded-lg border border-neutral-800 bg-neutral-900 p-5",
                    h2 { class: "text-base font-semibold text-white mb-1", "The unit-detection workflow" }
                    p { class: "text-xs text-neutral-500 mb-4",
                        "Follow these steps in order to turn raw game screenshots into a "
                        "model that identifies units from a screenshot. Each step's page "
                        "shows a banner with the previous and next step."
                    }
                    ol { class: "space-y-3",
                        for (i, step) in workflow::steps().iter().enumerate() {
                            li { key: "{i}", class: "flex gap-3 text-sm",
                                span { class: "shrink-0 w-5 h-5 mt-0.5 rounded-full bg-neutral-800 text-amber-400 text-xs font-semibold flex items-center justify-center",
                                    "{i + 1}"
                                }
                                div {
                                    match &step.route {
                                        Some(route) => rsx! {
                                            Link { class: "text-blue-400 hover:underline font-medium", to: route.clone(), "{step.name}" }
                                        },
                                        None => rsx! {
                                            span { class: "text-neutral-500 font-medium", "{step.name}" }
                                            span { class: "ml-2 px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-500 text-[10px]", "not built yet" }
                                        },
                                    }
                                    p { class: "text-xs text-neutral-500 mt-0.5", "{step.description}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
