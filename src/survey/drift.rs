//! Drift: a loaded model measured well above its declared footprint. Reported, never refused.

use core::fmt;
use std::collections::BTreeMap;

use super::Measured;
use crate::footprint::{Footprint, Vram};

// The spec's figure: drift is a measurement more than 10% above the declared one.
const DRIFT_OVER_PERCENT: u128 = 10;

/// Whether a measured figure is more than [`DRIFT_OVER_PERCENT`] above its declared one
#[cfg(test)]
pub(crate) fn drifts(declared: Footprint, measured: Measured) -> bool {
    over(declared, measured).any()
}

/// Which figures of `measured` are more than [`DRIFT_OVER_PERCENT`] over their declared ones
///
/// A figure declared `all`, or left unmeasured, is never over. VRAM declared as none counts as
/// 0, so any measured VRAM is over.
fn over(declared: Footprint, measured: Measured) -> Over {
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
    Over {
        vram,
        ram: over(declared.ram, measured.ram),
    }
}

/// Which of a model's figures a survey read
///
/// A figure left unread, because `nvidia-smi`, the flock or the model's ollama did not answer,
/// is unknown rather than unmeasured, so it keeps the drift it had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Read {
    /// The VRAM figure.
    pub vram: bool,
    /// The RAM figure.
    pub ram: bool,
}

impl Read {
    /// Both figures.
    #[cfg(test)]
    pub(crate) const BOTH: Read = Read {
        vram: true,
        ram: true,
    };
}

/// Which of a model's figures are over their declared ones
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Over {
    vram: bool,
    ram: bool,
}

impl Over {
    fn any(self) -> bool {
        self.vram || self.ram
    }
}

/// The models, or bare leases, drifting as of the last survey, and in which figures
#[derive(Debug)]
pub(crate) struct Drifting<K>(BTreeMap<K, Over>);

impl<K> Default for Drifting<K> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: Ord + Clone + fmt::Display> Drifting<K> {
    /// Takes this survey's figures and returns a log line for each key that started or stopped drifting
    ///
    /// A figure `now` marks unread keeps the drift it had. A key missing from `now` is
    /// forgotten without a line.
    pub fn update(&mut self, now: &BTreeMap<K, (Footprint, Measured, Read)>) -> Vec<String> {
        let mut lines = Vec::new();
        for (key, (declared, measured, read)) in now {
            let was = self.0.get(key).copied().unwrap_or_default();
            let found = over(*declared, *measured);
            let is = Over {
                vram: if read.vram { found.vram } else { was.vram },
                ram: if read.ram { found.ram } else { was.ram },
            };
            match (was.any(), is.any()) {
                (false, true) => lines.push(format!(
                    "paddock: {key} is drifting: it measures {} against {} declared",
                    measured_text(*measured),
                    declared_text(*declared)
                )),
                (true, false) => lines.push(format!(
                    "paddock: {key} is back within its declared footprint"
                )),
                _ => {}
            }
            if is.any() {
                self.0.insert(key.clone(), is);
            } else {
                self.0.remove(key);
            }
        }
        self.0.retain(|key, _| now.contains_key(key));
        lines
    }

    /// Forgets `key`'s drift without a line, so its next load starts from none
    pub fn forget(&mut self, key: &K) {
        self.0.remove(key);
    }

    /// Whether `key` was drifting at the last update
    pub fn contains(&self, key: &K) -> bool {
        self.0.contains_key(key)
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
