use faf_units::Unit;
use thiserror::Error;
pub type Result<T> = anyhow::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unit not found from search: {0}")]
    UnitNotFound(String),
    #[error("unit {0} failed to load unit cost")]
    UnitMustHasEcoCost(Box<Unit>),
    #[error("unit {0} should have ecnonomy")]
    UnitShouldHaveEconomy(Box<Unit>),
    #[error("unit {0} failed to find tech level")]
    UnitMustHasTechLevel(Box<Unit>),
    #[error("{0}")]
    Others(String),
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        Self::Others(value.to_string())
    }
}
