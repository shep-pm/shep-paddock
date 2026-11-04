//! What a meter reading hands the engine, and what the engine says it did with it.

use tokio::time::Instant;

use crate::{config::ModelName, meter::Reading, meter::Source};

/// One meter reading, as the engine takes it
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metered {
    /// The host's power.
    pub reading: Reading,
    /// When it was read.
    pub at: Instant,
    /// What the host pays per kWh at that moment, if it has a price.
    pub usd_per_kwh: Option<f64>,
}

/// Whom a share of energy was used for
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Use {
    /// Leases with a request in flight.
    Lease,
    /// The host being on with none.
    Idle,
}

impl Use {
    /// The `use` label's value.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::Idle => "idle",
        }
    }
}

/// Energy a reading added, to one model's leases or to nobody
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Share {
    /// The model whose leases used it, `None` for idle.
    pub model: Option<ModelName>,
    /// Whether leases used it or the host sat idle.
    pub used: Use,
    /// Its energy.
    pub joules: f64,
}

/// What a released lease used while it was held
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Used {
    /// Watt-hours.
    pub wh: f64,
    /// US dollars at each reading's price, or `None` while the host has no price.
    pub usd: Option<f64>,
    /// What measured the power.
    pub source: Source,
}
