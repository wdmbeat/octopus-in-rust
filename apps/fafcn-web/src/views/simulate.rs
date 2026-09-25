use dioxus::prelude::*;
use faf_blueprints::{ConstructionAction, ConstructionPlan};
use faf_sim_protocol::{EcoSnapshot, SimCmd, SimEvent};
use gloo_net::http::Request;

use crate::components::{
    to_sim_speed, use_sim_connection, AssignmentTarget, EcoChart, EcoPanel, EcoStats,
    JsonPlanEditor, QueueItemCreator, QueueItemList, SimulationControls, SimulationStatus,
    UnitSelectorModal, UnitSummary,
};
use crate::i18n::{self, Text};
use crate::state::{load_plan_from_storage, save_plan_to_storage};

const DEFAULT_PLAN: &str = r#"{
  "player_eco": {
    "mass_generate_rate": 2.0,
    "mass_drain": 0.0,
    "energy_generate_rate": 20.0,
    "energy_drain": 0.0,
    "mass_in_storage": 650.0,
    "max_capacity_in_mass_storage": 650.0,
    "energy_in_storage": 4000.0,
    "max_capacity_in_energy_storage": 4000.0
  },
  "building_queue": []
}"#;

fn default_plan() -> ConstructionPlan {
    serde_json::from_str(DEFAULT_PLAN).unwrap_or_default()
}

#[component]
pub fn Simulate() -> Element {
    let units = use_resource(move || async move {
        Request::get(&crate::net::api_url("/api/units"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<Vec<UnitSummary>>()
            .await
            .map_err(|e| e.to_string())
    });

    let mut plan = use_signal(|| load_plan_from_storage().unwrap_or_else(default_plan));
    let mut draft_builder = use_signal(|| None::<UnitSummary>);
    let mut draft_builder_count = use_signal(|| 1_u32);
    let mut draft_target = use_signal(|| None::<UnitSummary>);
    let mut draft_target_count = use_signal(|| 1_u32);
    let mut pending_target = use_signal(|| None::<AssignmentTarget>);
    let mut show_json_editor = use_signal(|| false);
    let mut status = use_signal(|| SimulationStatus::Idle);
    let speed = use_signal(|| 0.0_f64);
    let mut latest_eco = use_signal(|| None::<EcoSnapshot>);
    let mut chart_data = use_signal(Vec::<EcoSnapshot>::new);
    let mut status_msg = use_signal(String::new);
    let mut sim_time = use_signal(|| 0.0_f64);
    let mut connection = use_sim_connection();
    let t = i18n::use_t();

    // Persist plan changes to localStorage.
    use_effect(move || {
        save_plan_to_storage(&plan.read());
    });

    // Forward speed changes to a running simulation.
    use_effect(move || {
        let current_speed = to_sim_speed(*speed.read());
        if let Some(conn) = connection.read().as_ref() {
            conn.send_command(SimCmd::GameSpeed(current_speed));
        }
    });

    let unit_list = match units.read().as_ref() {
        Some(Ok(list)) => list.clone(),
        Some(Err(err)) => {
            return rsx! {
                div { class: "flex items-center justify-center h-full text-red-400",
                    "{t.t(Text::LoadUnitsFailed)}{err}"
                }
            };
        }
        None => {
            return rsx! {
                div { class: "flex items-center justify-center h-full text-neutral-400",
                    "{t.t(Text::Loading)}"
                }
            };
        }
    };

    let assign_unit = move |unit: UnitSummary| {
        if let Some(target) = *pending_target.read() {
            match target {
                AssignmentTarget::ExistingBuilder { start, end } => {
                    let blueprint = unit.to_blueprint();
                    plan.with_mut(|p| {
                        let mut queue = p.building_queue().to_vec();
                        let start = start as usize;
                        let end = (end as usize).min(queue.len());
                        for action in queue.iter_mut().take(end).skip(start) {
                            action.set_builders(vec![blueprint.clone()]);
                        }
                        *p = ConstructionPlan::new(*p.player_eco(), queue);
                    });
                }
                AssignmentTarget::ExistingTarget { start, end } => {
                    let blueprint = unit.to_blueprint();
                    plan.with_mut(|p| {
                        let mut queue = p.building_queue().to_vec();
                        let start = start as usize;
                        let end = (end as usize).min(queue.len());
                        for action in queue.iter_mut().take(end).skip(start) {
                            action.set_target(blueprint.clone());
                        }
                        *p = ConstructionPlan::new(*p.player_eco(), queue);
                    });
                }
                AssignmentTarget::NewBuilder => draft_builder.set(Some(unit)),
                AssignmentTarget::NewTarget => draft_target.set(Some(unit)),
            }
        }
        pending_target.set(None);
    };

    let save_draft = move |_| {
        let builder = draft_builder.read().clone();
        let target = draft_target.read().clone();
        if let (Some(builder), Some(target)) = (builder, target) {
            let builder_count = (*draft_builder_count.read()).max(1);
            let target_count = (*draft_target_count.read()).max(1);
            let builder_blueprint = builder.to_blueprint();
            let target_blueprint = target.to_blueprint();
            let builders = vec![builder_blueprint; builder_count as usize];

            plan.with_mut(|p| {
                let mut queue = p.building_queue().to_vec();
                for _ in 0..target_count {
                    queue.push(ConstructionAction::new(
                        builders.clone(),
                        target_blueprint.clone(),
                    ));
                }
                *p = ConstructionPlan::new(*p.player_eco(), queue);
            });

            draft_builder.set(None);
            draft_target.set(None);
            draft_builder_count.set(1);
            draft_target_count.set(1);
        }
    };

    let clear_draft = move |_| {
        draft_builder.set(None);
        draft_target.set(None);
        draft_builder_count.set(1);
        draft_target_count.set(1);
    };

    let on_start = move |_| {
        chart_data.write().clear();
        status.set(SimulationStatus::Running);
        status_msg.set("connecting...".to_string());

        let mut status_writer = status;
        let mut eco_writer = latest_eco;
        let mut data_writer = chart_data;
        let mut msg_writer = status_msg;
        let mut conn_writer = connection;
        let mut conn_status_writer = connection;

        match crate::components::SimConnection::open(
            plan.read().clone(),
            to_sim_speed(*speed.read()),
            move |event| match event {
                SimEvent::EcoSummary(snapshot) => {
                    sim_time.set(snapshot.time);
                    data_writer.write().push(snapshot);
                    eco_writer.set(Some(snapshot));
                }
                SimEvent::ActionFinished(_) => {
                    msg_writer.set("action finished".to_string());
                }
            },
            move |msg| {
                if msg == "finished" {
                    status_writer.set(SimulationStatus::Finished);
                    conn_status_writer.set(None);
                }
                msg_writer.set(msg);
            },
        ) {
            Ok(conn) => conn_writer.set(Some(conn)),
            Err(err) => {
                msg_writer.set(format!("failed to connect: {err:?}"));
                status_writer.set(SimulationStatus::Idle);
            }
        }
    };

    let on_pause = move |_| {
        if let Some(conn) = connection.read().as_ref() {
            conn.send_command(SimCmd::Pause);
            status.set(SimulationStatus::Paused);
        }
    };

    let on_resume = move |_| {
        if let Some(conn) = connection.read().as_ref() {
            conn.send_command(SimCmd::Resume);
            status.set(SimulationStatus::Running);
        }
    };

    let on_stop = move |_| {
        if let Some(conn) = connection.read().as_ref() {
            conn.send_command(SimCmd::Stop);
        }
        status.set(SimulationStatus::Idle);
    };

    let on_reset = move |_| {
        if let Some(conn) = connection.read().as_ref() {
            conn.close();
        }
        connection.set(None);
        chart_data.write().clear();
        latest_eco.set(None);
        sim_time.set(0.0);
        status.set(SimulationStatus::Idle);
        status_msg.set(String::new());
    };

    let plan_locked = matches!(
        *status.read(),
        SimulationStatus::Running | SimulationStatus::Paused
    );

    rsx! {
        div { class: "flex flex-col flex-1 min-h-0 bg-neutral-950 text-gray-200 overflow-hidden select-none font-sans",
            div { class: "flex flex-1 overflow-hidden",
                // Left sidebar: eco settings + new item creator.
                div {
                    class: if plan_locked { "w-80 shrink-0 overflow-auto p-4 border-r border-neutral-800 bg-neutral-900/30 pointer-events-none opacity-60" } else { "w-80 shrink-0 overflow-auto p-4 border-r border-neutral-800 bg-neutral-900/30" },
                    EcoPanel { plan, disabled: plan_locked }
                    div { class: "my-4 border-t border-neutral-700" }
                    QueueItemCreator {
                        draft_builder,
                        draft_builder_count,
                        draft_target,
                        draft_target_count,
                        disabled: plan_locked,
                        on_assign_slot: move |target: AssignmentTarget| pending_target.set(Some(target)),
                        on_save: save_draft,
                        on_clear: clear_draft,
                    }
                }

                // Right area: queue (top) + simulation (bottom).
                div { class: "flex-1 overflow-hidden flex flex-col",
                    div { class: "flex-1 overflow-hidden flex flex-col p-4 border-b border-neutral-800 bg-neutral-900/30",
                        div { class: "flex items-center gap-2 mb-3 shrink-0",
                            h3 { class: "text-sm font-semibold text-white", "{t.t(Text::ConstructionPlan)}" }
                            button {
                                class: "px-2 py-1 text-xs rounded bg-blue-600 hover:bg-blue-500 text-white transition-colors font-mono shadow-sm",
                                title: if *show_json_editor.read() { t.t(Text::ShowCards) } else { t.t(Text::ShowJson) },
                                onclick: move |_| show_json_editor.set(!show_json_editor()),
                                if *show_json_editor.read() { "☰" } else { "{{ }}" }
                            }
                        }
                        if *show_json_editor.read() {
                            JsonPlanEditor { plan, disabled: plan_locked }
                        } else {
                            QueueItemList {
                                plan,
                                disabled: plan_locked,
                                on_assign_slot: move |target: AssignmentTarget| pending_target.set(Some(target)),
                            }
                        }
                    }

                    div { class: "flex-1 overflow-hidden flex flex-col p-4 bg-neutral-900/30",
                        div { class: "flex items-center mb-3 shrink-0",
                            div { class: "flex-1" }
                            SimulationControls {
                                status,
                                speed,
                                on_start,
                                on_pause,
                                on_resume,
                                on_stop,
                                on_reset,
                            }
                            div { class: "flex-1 flex items-center justify-end gap-3",
                                div { class: "text-xs text-neutral-400", "{status_msg}" }
                                div { class: "text-sm text-neutral-400 tabular-nums",
                                    if *status.read() == SimulationStatus::Finished {
                                        "{sim_time:.1}s / {sim_time:.1}s"
                                    } else {
                                        "{sim_time:.1}s / ..."
                                    }
                                }
                            }
                        }
                        div { class: "flex-1 min-h-0 flex flex-col",
                            EcoStats { eco: latest_eco }
                            div { class: "flex-1 border border-neutral-700 rounded-lg bg-neutral-900/80 p-3 mt-3 min-h-0 flex flex-col",
                                EcoChart { data: chart_data, latest: *latest_eco.read() }
                            }
                        }
                    }
                }
            }
        }

        UnitSelectorModal {
            open: pending_target.read().is_some(),
            units: unit_list,
            target: (*pending_target.read()).unwrap_or(AssignmentTarget::NewTarget),
            on_select: assign_unit,
            on_close: move |_| pending_target.set(None),
        }
    }
}
