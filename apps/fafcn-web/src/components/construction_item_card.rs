use dioxus::prelude::*;
use faf_blueprints::{ConstructionAction, ConstructionPlan, UnitBlueprint};

use crate::components::{AssignmentTarget, UnitBlock, UnitSummary};
use crate::i18n::{self, Text};

#[component]
pub fn ConstructionItemCard(
    start_index: usize,
    end_index: usize,
    item: ConstructionAction,
    plan: Signal<ConstructionPlan>,
    on_assign_slot: EventHandler<AssignmentTarget>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let group_size = end_index - start_index;
    let display_index = start_index + 1;

    let remove = move |_| {
        if !disabled {
            plan.with_mut(|p| {
                let mut queue = p.building_queue().to_vec();
                if start_index < queue.len() {
                    queue.drain(start_index..end_index.min(queue.len()));
                    *p = ConstructionPlan::new(*p.player_eco(), queue);
                }
            });
        }
    };

    let adjust_builders = move |new_len: u32| {
        if disabled {
            return;
        }
        plan.with_mut(|p| {
            let mut queue = p.building_queue().to_vec();
            let end = end_index.min(queue.len());
            for action in queue.iter_mut().take(end).skip(start_index) {
                let template = action.builders().first().cloned();
                let mut builders = action.builders().to_vec();
                resize_with_template(&mut builders, new_len as usize, template);
                action.set_builders(builders);
            }
            *p = ConstructionPlan::new(*p.player_eco(), queue);
        });
    };

    let adjust_targets = move |new_len: u32| {
        if disabled {
            return;
        }
        plan.with_mut(|p| {
            let mut queue = p.building_queue().to_vec();
            if start_index >= queue.len() {
                return;
            }
            let current_end = (start_index + group_size).min(queue.len());
            let current_len = current_end - start_index;
            let template = queue[start_index].clone();
            match new_len.cmp(&(current_len as u32)) {
                std::cmp::Ordering::Greater => {
                    let extra = (new_len as usize) - current_len;
                    let insert_at = current_end;
                    for _ in 0..extra {
                        queue.insert(insert_at, template.clone());
                    }
                }
                std::cmp::Ordering::Less => {
                    let keep = new_len as usize;
                    queue.drain((start_index + keep)..current_end);
                }
                std::cmp::Ordering::Equal => {}
            }
            *p = ConstructionPlan::new(*p.player_eco(), queue);
        });
    };

    let builder_summary = item.builders().first().map(UnitSummary::from_blueprint);
    let target_summary = Some(UnitSummary::from_blueprint(item.target()));
    let builder_count = item.builders().len() as u32;
    let target_count = group_size as u32;
    let t = i18n::use_t();

    rsx! {
        div { class: "w-full p-2 rounded-lg bg-neutral-800/50 border border-neutral-700 text-sm",
            div { class: "flex items-center justify-between mb-1.5",
                span { class: "text-[10px] uppercase tracking-wide text-neutral-500", "{t.t(Text::QueueItemPrefix)}{display_index}" }
                button {
                    class: if disabled { "px-2 py-0.5 rounded bg-red-900/20 text-red-300/50 text-xs cursor-not-allowed" } else { "px-2 py-0.5 rounded bg-red-900/40 hover:bg-red-900/60 text-red-300 text-xs transition-colors" },
                    disabled,
                    onclick: remove,
                    "x"
                }
            }
            div { class: "flex flex-col gap-1.5",
                UnitBlock {
                    label: t.t(Text::Builder).to_string(),
                    unit: builder_summary,
                    count: builder_count,
                    hint: t.t(Text::HintBuildPower).to_string(),
                    disabled,
                    on_click: move |_| on_assign_slot.call(AssignmentTarget::ExistingBuilder { start: start_index as u32, end: end_index as u32 }),
                    on_count: adjust_builders,
                }
                UnitBlock {
                    label: t.t(Text::Target).to_string(),
                    unit: target_summary,
                    count: target_count,
                    hint: t.t(Text::HintDropAny).to_string(),
                    disabled,
                    on_click: move |_| on_assign_slot.call(AssignmentTarget::ExistingTarget { start: start_index as u32, end: end_index as u32 }),
                    on_count: adjust_targets,
                }
            }
        }
    }
}

fn resize_with_template(
    vec: &mut Vec<UnitBlueprint>,
    new_len: usize,
    template: Option<UnitBlueprint>,
) {
    if vec.len() == new_len {
        return;
    }
    if new_len == 0 || template.is_none() {
        vec.clear();
        return;
    }
    let template = template.unwrap();
    while vec.len() < new_len {
        vec.push(template.clone());
    }
    vec.truncate(new_len);
}
