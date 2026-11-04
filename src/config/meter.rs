//! The `[meter]` table: where the host's power is read, and how often.

use core::time::Duration;

use super::{ConfigError, section::MeterSection, values::duration_or};

const DEFAULT_EVERY: Duration = Duration::from_secs(5);
/// An HS300 has six outlets, numbered from 0.
const LAST_OUTLET: u8 = 5;

/// A price in US dollars per kWh: finite, and zero or more
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct UsdPerKwh(f64);

// A price is never NaN, so equality is reflexive.
impl Eq for UsdPerKwh {}

impl UsdPerKwh {
    /// The price, in dollars.
    pub(crate) const fn get(self) -> f64 {
        self.0
    }
}

/// How the host's power is read, and priced
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeterConfig {
    /// The Kasa plug or strip to ask. `None` sums the GPU's and the CPU package's own readings.
    pub kasa: Option<String>,
    /// The strip's outlet the host is plugged into. `None` for a single plug.
    pub outlet: Option<u8>,
    /// How often to read.
    pub every: Duration,
    /// The price from the maintainer's bill. It wins over a lookup.
    pub usd_per_kwh: Option<UsdPerKwh>,
    /// The environment variable holding the EIA API key, for the lookup.
    pub eia_key_env: Option<String>,
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
    if section.eia_key_env.as_deref() == Some("") {
        return Err(ConfigError::EmptyEiaKeyEnv);
    }
    let usd_per_kwh = section
        .usd_per_kwh
        .map(|price| {
            if price.is_finite() && price >= 0.0 {
                Ok(UsdPerKwh(price))
            } else {
                Err(ConfigError::BadElectricityPrice {
                    value: price.to_string(),
                })
            }
        })
        .transpose()?;
    Ok(MeterConfig {
        kasa: section.kasa,
        outlet: section.outlet,
        every,
        usd_per_kwh,
        eia_key_env: section.eia_key_env,
    })
}
