//! Drift: a loaded model measured well above its declared footprint. Reported, never refused.

use std::collections::{BTreeMap, BTreeSet};

use super::Measured;
use crate::{
    config::ModelName,
    footprint::{Footprint, Vram},
};

// The spec's figure: drift is a measurement more than 10% above the declared one.
const DRIFT_OVER_PERCENT: u128 = 10;

/// Whether a measured figure is more than [`DRIFT_OVER_PERCENT`] above its declared one
///
/// A figure declared `all`, or left unmeasured, never drifts. VRAM declared as none counts as 0,
/// so any measured VRAM drifts.
pub(crate) fn drifts(declared: Footprint, measured: Measured) -> bool {
    let over = |declared: u64, measured: Option<u64>| {
        measured.is_some_and(|measured| {
            u128::from(measured) * 100 > u128::from(declared) * (100 + DRIFT_OVER_PERCENT)
        })
    };
    let vram = match declared.vram {
        Vram::All => false,
        Vram::None => over(0, measured.vram),
        Vram::Bytes(bytes) => over(bytes, measured.vram),
    };
    vram || over(declared.ram, measured.ram)
}

/// The models drifting as of the last survey
#[derive(Debug, Default)]
pub(crate) struct Drifting(BTreeSet<ModelName>);

impl Drifting {
    /// Takes this survey's figures and returns a log line for each model that started or stopped drifting
    ///
    /// A model missing from `now` is forgotten without a line.
    pub fn update(&mut self, now: &BTreeMap<ModelName, (Footprint, Measured)>) -> Vec<String> {
        let mut lines = Vec::new();
        for (model, (declared, measured)) in now {
            if drifts(*declared, *measured) {
                if self.0.insert(model.clone()) {
                    lines.push(format!(
                        "paddock: {model} is drifting: it measures {} against {} declared",
                        measured_text(*measured),
                        declared_text(*declared)
                    ));
                }
            } else if self.0.remove(model) {
                lines.push(format!(
                    "paddock: {model} is back within its declared footprint"
                ));
            }
        }
        self.0.retain(|model| now.contains_key(model));
        lines
    }

    /// Whether `model` was drifting at the last update
    pub fn contains(&self, model: &ModelName) -> bool {
        self.0.contains(model)
    }
}

/// `7000 MiB VRAM and 1504 MiB RAM`, with `unmeasured` for a figure not measured
fn measured_text(measured: Measured) -> String {
    let mib = |bytes: Option<u64>| {
        bytes.map_or_else(
            || "unmeasured".to_owned(),
            |bytes| format!("{} MiB", bytes >> 20),
        )
    };
    format!("{} VRAM and {} RAM", mib(measured.vram), mib(measured.ram))
}

/// `6144 MiB VRAM and 2048 MiB RAM`, with `all` and `no` for those VRAM declarations
fn declared_text(declared: Footprint) -> String {
    let vram = match declared.vram {
        Vram::All => "all".to_owned(),
        Vram::None => "no".to_owned(),
        Vram::Bytes(bytes) => format!("{} MiB", bytes >> 20),
    };
    format!("{vram} VRAM and {} MiB RAM", declared.ram >> 20)
}
