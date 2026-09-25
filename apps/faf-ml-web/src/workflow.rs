//! The unit-detection workflow: the ordered path from raw screenshots to a
//! trained detector. Single source of truth — the Home page renders it as a
//! linked checklist, and each step's page shows a prev/next banner so users
//! can follow the workflow straight through.

use dioxus::prelude::*;

use crate::Route;

/// One step of the workflow.
#[derive(Clone)]
pub struct WorkflowStep {
    pub name: &'static str,
    pub description: &'static str,
    /// Where the step is performed; `None` = not built yet (rendered muted).
    pub route: Option<Route>,
}

/// The ordered workflow (routes aren't const-constructible, hence a fn).
pub fn steps() -> Vec<WorkflowStep> {
    vec![
        WorkflowStep {
            name: "Configure unit icons",
            description: "Enable strategic-icon mods like in game, preview each \
                          unit's effective icon, and pick the icon classes the \
                          detector trains on.",
            route: Some(Route::Icons {}),
        },
        WorkflowStep {
            name: "Collect & triage screenshots",
            description: "Upload game screenshots, then mark each: background \
                          (empty terrain — the datagen canvas) or battle (real \
                          units — the held-out test pool).",
            route: Some(Route::Gallery {}),
        },
        WorkflowStep {
            name: "Generate synthetic samples",
            description: "Paste strategic-icon sprites onto background crops — \
                          perfectly labeled training data for free.",
            route: Some(Route::Datagen {}),
        },
        WorkflowStep {
            name: "Review labels",
            description: "Open any image from the Gallery: fix box classes, \
                          delete wrong boxes, save.",
            route: Some(Route::Gallery {}),
        },
        WorkflowStep {
            name: "Snapshot a dataset",
            description: "Freeze a reviewed batch into an immutable snapshot — \
                          labels are embedded and never change.",
            route: Some(Route::Datasets {}),
        },
        WorkflowStep {
            name: "Train & monitor",
            description: "Train the detector and watch loss / mAP live \
                          (currently a dummy pipeline).",
            route: Some(Route::Training {}),
        },
        WorkflowStep {
            name: "Evaluate on real screenshots",
            description: "Run the trained model on held-out battle shots — the \
                          moment of truth (CLI for now; UI coming).",
            route: None,
        },
    ]
}

/// Prev/next banner shown at the top of a workflow step's page.
/// `step` is 1-based (index into `steps()` + 1).
#[component]
pub fn WorkflowBanner(step: usize) -> Element {
    let all = steps();
    let total = all.len();
    let current = &all[step.saturating_sub(1).min(total - 1)];
    let prev = (step > 1).then(|| &all[step - 2]);
    let next = (step < total).then(|| &all[step]);

    rsx! {
        div { class: "mb-4 flex items-center gap-3 rounded-lg border border-neutral-800 bg-neutral-900/60 px-4 py-2 text-xs",
            span { class: "text-amber-400 font-semibold shrink-0", "Step {step}/{total}" }
            span { class: "text-neutral-300", "{current.name}" }
            div { class: "flex-1" }
            if let Some(prev) = prev {
                if let Some(route) = &prev.route {
                    Link { class: "text-blue-400 hover:underline shrink-0", to: route.clone(), "← {prev.name}" }
                }
            }
            if let Some(next) = next {
                match &next.route {
                    Some(route) => rsx! {
                        Link { class: "text-blue-400 hover:underline shrink-0", to: route.clone(), "{next.name} →" }
                    },
                    None => rsx! {
                        span { class: "text-neutral-600 shrink-0", "{next.name} (not built yet)" }
                    },
                }
            }
        }
    }
}
