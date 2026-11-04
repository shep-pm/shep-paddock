//! The `[meter]` table: where the host's power is read, and how often.

use core::time::Duration;

use super::{ConfigError, section::MeterSection, values::duration_or};

const DEFAULT_EVERY: Duration = Duration::from_secs(5);
/// An HS300 has six outlets, numbered from 0.
const LAST_OUTLET: u8 = 5;

/// How the host's power is read
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeterConfig {
    /// The Kasa plug or strip to ask. `None` sums the GPU's and the CPU package's own readings.
    pub kasa: Option<String>,
    /// The strip's outlet the host is plugged into. `None` for a single plug.
    pub outlet: Option<u8>,
    /// How often to read.
    pub every: Duration,
}

pub(super) fn build_meter(section: MeterSection) -> Result<MeterConfig, ConfigError> {
    let every = duration_or(section.every.as_deref(), "meter.every", DEFAULT_EVERY)?;
    if every.is_zero() {
        return Err(ConfigError::ZeroMeterInterval);
    }
    if section.kasa.as_deref() == Some("") {
        return Err(ConfigError::EmptyMeterHost);
    }
    match (&section.kasa, section.outlet) {
        (None, Some(_)) => return Err(ConfigError::MeterOutletWithoutKasa),
        (_, Some(outlet)) if outlet > LAST_OUTLET => {
            return Err(ConfigError::MeterOutletOutOfRange { outlet });
        }
        _ => {}
    }
    Ok(MeterConfig {
        kasa: section.kasa,
        outlet: section.outlet,
        every,
    })
}
