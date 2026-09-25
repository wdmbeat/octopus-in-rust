use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Copy, Deserialize, Serialize)]
pub struct PlayerEcoMetrics {
    // mass produce vs consume
    pub mass_generate_rate: f64,
    pub mass_drain: f64,

    // energy produce vs consume
    pub energy_generate_rate: f64,
    pub energy_drain: f64,

    // continuous energy maintenance paid by all owned units
    #[serde(default)]
    pub maintenance_consumption_per_second_energy: f64,

    // cumulative resources spent on construction
    #[serde(default)]
    pub total_mass_spent: f64,
    #[serde(default)]
    pub total_energy_spent: f64,

    // storage related
    pub mass_in_storage: f64,
    pub max_capacity_in_mass_storage: f64,
    pub energy_in_storage: f64,
    pub max_capacity_in_energy_storage: f64,
}

impl std::fmt::Debug for PlayerEcoMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PlayerEcoMetrics {{ mass_generate_rate: {:.1}, mass_drain: {:.1}, energy_generate_rate: {:.1}, energy_drain: {:.1}, maintenance_consumption_per_second_energy: {:.1}, total_mass_spent: {:.1}, total_energy_spent: {:.1}, mass_in_storage: {:.1}, max_capacity_in_mass_storage: {:.1}, energy_in_storage: {:.1}, max_capacity_in_energy_storage: {:.1} }}",
            self.mass_generate_rate,
            self.mass_drain,
            self.energy_generate_rate,
            self.energy_drain,
            self.maintenance_consumption_per_second_energy,
            self.total_mass_spent,
            self.total_energy_spent,
            self.mass_in_storage,
            self.max_capacity_in_mass_storage,
            self.energy_in_storage,
            self.max_capacity_in_energy_storage,
        )
    }
}

impl PlayerEcoMetrics {
    pub fn initial() -> Self {
        Self {
            mass_generate_rate: 1.0,
            mass_drain: 0.0,
            energy_generate_rate: 20.0,
            energy_drain: 0.0,
            maintenance_consumption_per_second_energy: 0.0,
            total_mass_spent: 0.0,
            total_energy_spent: 0.0,
            mass_in_storage: 650.0,
            max_capacity_in_mass_storage: 650.0,
            energy_in_storage: 4000.0,
            max_capacity_in_energy_storage: 4000.0,
        }
    }

    pub fn net_mass_rate(&mut self) -> f64 {
        self.mass_generate_rate - self.mass_drain
    }

    pub fn net_energy_rate(&self) -> f64 {
        self.energy_generate_rate
            - self.maintenance_consumption_per_second_energy
            - self.energy_drain
    }

    pub fn energy_efficiency(&self) -> f64 {
        if self.energy_in_storage > 0.0 {
            1.0
        } else {
            self.energy_generate_rate / self.energy_drain
        }
    }

    // need to consider energy_efficiency
    fn mass_efficiency(&self) -> f64 {
        if self.mass_in_storage > 0.0 {
            1.0
        } else {
            self.mass_generate_rate / self.mass_drain
        }
    }

    // direct efficiency apply to update build progress
    pub fn construction_efficiency(&self) -> f64 {
        self.mass_efficiency() * self.energy_efficiency()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Copy, PartialEq)]
pub struct UnitCostMetrics {
    pub mass: f64,
    pub energy: f64,
    pub build_time: f64,
}

impl UnitCostMetrics {
    pub fn new(mass: f64, energy: f64, build_time: f64) -> Self {
        UnitCostMetrics {
            mass,
            energy,
            build_time,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UnitEffectEcoMetrics {
    pub generate_mass_rate: f64,
    pub generate_energy_rate: f64,
    pub maintainance_energy_drain: f64,
    pub increase_mass_storage_capacity: f64,
    pub increase_energy_storage_capacity: f64,
    pub build_power: f64,
}

impl UnitEffectEcoMetrics {
    pub fn new(
        generate_mass_rate: f64,
        generate_energy_rate: f64,
        maintainance_energy_drain: f64,
        increase_mass_storage_capacity: f64,
        increase_energy_storage_capacity: f64,
        build_power: f64,
    ) -> Self {
        UnitEffectEcoMetrics {
            generate_mass_rate,
            generate_energy_rate,
            maintainance_energy_drain,
            increase_mass_storage_capacity,
            increase_energy_storage_capacity,
            build_power,
        }
    }
}
