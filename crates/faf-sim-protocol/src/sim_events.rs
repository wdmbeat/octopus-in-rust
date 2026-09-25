use faf_blueprints::{ConstructionPlan, PlayerEcoMetrics};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A rich economy snapshot emitted every simulation tick.
///
/// This is a superset of [`PlayerEcoMetrics`] that adds the current simulation
/// time and separates maintenance drain from construction drain so the frontend
/// can draw FAF-style budget charts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EcoSnapshot {
    pub time: f64,
    pub production_per_second_mass: f64,
    pub production_per_second_energy: f64,
    pub maintenance_consumption_per_second_energy: f64,
    pub mass_drain: f64,
    pub energy_drain: f64,
    pub total_mass_spent: f64,
    pub total_energy_spent: f64,
    pub mass_storage: f64,
    pub mass_storage_cap: f64,
    pub energy_storage: f64,
    pub energy_storage_cap: f64,
}

impl EcoSnapshot {
    pub fn from_player_eco(time: f64, eco: &PlayerEcoMetrics) -> Self {
        Self {
            time,
            production_per_second_mass: eco.mass_generate_rate,
            production_per_second_energy: eco.energy_generate_rate,
            maintenance_consumption_per_second_energy: eco
                .maintenance_consumption_per_second_energy,
            mass_drain: eco.mass_drain,
            energy_drain: eco.energy_drain,
            total_mass_spent: eco.total_mass_spent,
            total_energy_spent: eco.total_energy_spent,
            mass_storage: eco.mass_in_storage,
            mass_storage_cap: eco.max_capacity_in_mass_storage,
            energy_storage: eco.energy_in_storage,
            energy_storage_cap: eco.max_capacity_in_energy_storage,
        }
    }
}

/// Commands sent from the service / CLI into a running simulation thread.
///
/// These cross the normal-application → Bevy-app boundary via the
/// `crossbeam_channel` held by `SimulationHandle` in `faf-sim-service`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimCmd {
    Start,
    Pause,
    Resume,
    /// Stop the simulation. The server will close the connection once the
    /// simulation thread exits.
    Stop,
    /// Change playback speed. The engine itself is tick-based; speed is
    /// realized by the service thread throttling how often it calls
    /// `app.update()`.
    GameSpeed(SimSpeed),
}

/// Simulation playback speed.
///
/// The engine processes one fixed tick per `app.update()`.  One tick
/// represents one simulation second.  Real-world cadence is controlled
/// by sleeping between ticks in `faf-sim-service::run_sim_thread`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SimSpeed {
    /// Run the simulation as fast as the CPU allows (default for headless runs).
    Unlimited,
    /// Run at a fixed number of simulation ticks per wall-clock second.
    /// In this mode one tick represents one simulation second.
    TicksPerSecond(f64),
}

impl Default for SimSpeed {
    /// Default speed used when none is specified.
    fn default() -> Self {
        SimSpeed::Unlimited
    }
}

impl SimSpeed {
    /// Number of wall-clock seconds to wait between ticks, if any.
    pub fn tick_interval_seconds(&self) -> Option<f64> {
        match self {
            SimSpeed::Unlimited => None,
            SimSpeed::TicksPerSecond(rate) => Some(1.0 / rate),
        }
    }
}

/// Events emitted by a running simulation back to the service / CLI.
///
/// These cross the Bevy-app → normal-application boundary via the
/// `EventSender` resource held by the engine world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimEvent {
    EcoSummary(EcoSnapshot),
    ActionFinished(Uuid),
}

impl std::fmt::Display for SimEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimEvent::EcoSummary(eco) => write!(f, "eco summary => {:?}", eco),
            SimEvent::ActionFinished(task_id) => write!(f, "action finished => {task_id}"),
        }
    }
}

/// Messages the frontend sends to the simulation server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimClientMessage {
    StartPlan {
        plan: ConstructionPlan,
        speed: SimSpeed,
    },
    Command(SimCmd),
}

/// Messages the simulation server sends to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimServerMessage {
    Event(SimEvent),
    Error(String),
    Finished,
}
