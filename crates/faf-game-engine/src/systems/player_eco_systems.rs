use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use faf_blueprints::UnitCostMetrics;
use std::collections::HashMap;
use uuid::Uuid;

type BuilderFilter = (With<BuildPower>, With<ConstructionBuilder>);
type TargetFilter = (With<UnitCost>, With<ConstructionTarget>);
type BuilderData<'w> = (&'w BuildPower, &'w ConstructionBuilder);
type TargetData<'w> = (&'w UnitCost, &'w ConstructionTarget);
type BuilderEntityData<'w> = (Entity, &'w BuildPower, &'w ConstructionBuilder);
type TargetEntityData<'w> = (Entity, &'w UnitCost, &'w ConstructionTarget);

// A system which aggregate all mass drain and energy drain from all building tasks
pub fn update_player_eco_from_building_units(
    mut player_eco: ResMut<PlayerEco>,
    construction_builder_query: Query<BuilderData, BuilderFilter>,
    construction_target_query: Query<TargetData, TargetFilter>,
    maintenance_query: Query<&MaintainancePowerDrain, With<MaintainancePowerDrain>>,
) {
    // Drain accumulates only from currently active construction tasks.
    player_eco.0.mass_drain = 0.0;
    player_eco.0.energy_drain = 0.0;

    let mut build_powers_tracking: HashMap<Uuid, f64> = HashMap::new();
    for (build_power, builder) in construction_builder_query {
        build_powers_tracking
            .entry(builder.task)
            .and_modify(|bp| *bp += build_power.0)
            .or_insert(build_power.0);
    }

    let mut build_cost_tracking: HashMap<Uuid, UnitCostMetrics> = HashMap::new();
    for (unit_cost, target) in construction_target_query {
        build_cost_tracking
            .entry(target.task)
            .insert_entry(unit_cost.0);
    }

    // based on each target, compute the mass drain and energy drain for each ConstructionTarget
    for (task, build_cost) in build_cost_tracking {
        let assigned_build_power = build_powers_tracking.get(&task).unwrap();
        let drain_ratio = build_cost.build_time / assigned_build_power;
        let mass_drain = build_cost.mass / drain_ratio;
        let energy_drain = build_cost.energy / drain_ratio;

        player_eco.0.mass_drain += mass_drain;
        player_eco.0.energy_drain += energy_drain;
    }

    // Sum continuous maintenance drains from all finished units.
    player_eco.0.maintenance_consumption_per_second_energy =
        maintenance_query.iter().map(|m| m.0).sum();

    // Track cumulative construction spending.
    player_eco.0.total_mass_spent += player_eco.0.mass_drain;
    player_eco.0.total_energy_spent += player_eco.0.energy_drain;

    let net_mass_rate = player_eco.0.net_mass_rate();
    let net_energy_rate = player_eco.0.net_energy_rate();

    let udpated_mass_in_storage = if player_eco.0.mass_in_storage + net_mass_rate > 0.0 {
        player_eco
            .0
            .max_capacity_in_mass_storage
            .min(player_eco.0.mass_in_storage + net_mass_rate)
    } else {
        0.0
    };

    let updated_energy_in_storage = if player_eco.0.energy_in_storage + net_energy_rate > 0.0 {
        player_eco
            .0
            .max_capacity_in_energy_storage
            .min(player_eco.0.energy_in_storage + net_energy_rate)
    } else {
        0.0
    };

    player_eco.0.mass_in_storage = udpated_mass_in_storage;
    player_eco.0.energy_in_storage = updated_energy_in_storage;
}

pub fn update_construction_pragress(
    player_eco: Res<PlayerEco>,
    mut commands: Commands,
    mut finished_construction_writer: MessageWriter<BuildingFinished>,
    construction_builder_query: Query<BuilderEntityData, BuilderFilter>,
    construction_target_query: Query<TargetEntityData, TargetFilter>,
) {
    let mut build_powers_tracking: HashMap<Uuid, f64> = HashMap::new();
    for (_, build_power, builder) in construction_builder_query {
        build_powers_tracking
            .entry(builder.task)
            .and_modify(|bp| *bp += build_power.0)
            .or_insert(build_power.0);
    }

    for (entity, unit_cost, target) in construction_target_query {
        let task_id = target.task;
        let current_progress = target.progress;

        let assigned_bp_for_task = build_powers_tracking.get(&task_id).unwrap();

        // overwrite to update target progress
        commands.entity(entity).insert(ConstructionTarget::new(
            task_id,
            current_progress + assigned_bp_for_task,
            target.unit_eco_effect.clone(),
            target.tech_level,
        ));

        if current_progress + assigned_bp_for_task > unit_cost.0.build_time {
            finished_construction_writer.write(BuildingFinished { task_id });

            commands.trigger(PlayerEcoSummary(player_eco.0));
        }
    }
}

pub fn apply_finished_constructions(
    mut player_eco: ResMut<PlayerEco>,
    mut finished_construction_reader: MessageReader<BuildingFinished>,
    mut commands: Commands,
    construction_builder_query: Query<(Entity, &ConstructionBuilder), With<ConstructionBuilder>>,
    construction_target_query: Query<(Entity, &ConstructionTarget), With<ConstructionTarget>>,
) {
    for finished_task in finished_construction_reader.read() {
        for (each_builder_entity, builder) in construction_builder_query {
            if builder.task == finished_task.task_id {
                commands
                    .entity(each_builder_entity)
                    .remove::<ConstructionBuilder>();
            }
        }

        for (each_target_entity, target) in construction_target_query {
            if target.task == finished_task.task_id {
                commands
                    .entity(each_target_entity)
                    .remove::<ConstructionTarget>();

                // spawn new entity and according to UnitEffectEcoMetrics assign valid effect
                let mut entity = commands.spawn_empty();

                if target.unit_eco_effect.build_power > 0.0 {
                    entity.insert(BuildPower(target.unit_eco_effect.build_power));
                }

                if target.unit_eco_effect.generate_mass_rate > 0.0 {
                    entity.insert(GenerateMass(target.unit_eco_effect.generate_mass_rate));
                    player_eco.0.mass_generate_rate += target.unit_eco_effect.generate_mass_rate;
                }

                if target.unit_eco_effect.generate_energy_rate > 0.0 {
                    entity.insert(GenerateEnergy(target.unit_eco_effect.generate_energy_rate));
                    player_eco.0.energy_generate_rate +=
                        target.unit_eco_effect.generate_energy_rate;
                }

                if target.unit_eco_effect.maintainance_energy_drain > 0.0 {
                    entity.insert(MaintainancePowerDrain(
                        target.unit_eco_effect.maintainance_energy_drain,
                    ));
                    // Maintenance is aggregated each tick from
                    // MaintainancePowerDrain components, not added to energy_drain.
                }

                if target.unit_eco_effect.increase_mass_storage_capacity > 0.0 {
                    entity.insert(IncreaseMassStorageCapacity(
                        target.unit_eco_effect.increase_mass_storage_capacity,
                    ));
                    player_eco.0.max_capacity_in_mass_storage +=
                        target.unit_eco_effect.increase_mass_storage_capacity;
                }

                if target.unit_eco_effect.increase_energy_storage_capacity > 0.0 {
                    entity.insert(IncreaseEnergyStorageCapacity(
                        target.unit_eco_effect.increase_energy_storage_capacity,
                    ));
                    player_eco.0.max_capacity_in_energy_storage +=
                        target.unit_eco_effect.increase_energy_storage_capacity;
                }

                entity.insert(UnitTechLevel(target.tech_level));
            }
        }
    }
}
