//! The host's power, read on a timer and sent to the metrics dog so the energy its leases use can
//! be counted.
//!
//! Two sources. A Kasa plug's energy meter reads what the wall supplies, losses included. With no
//! plug, the GPU's draw and the CPU package's power are summed, which leaves out RAM, the board,
//! the fans and the power supply's loss, so it is reported under its own `meter` value. Admission
//! never reads either: it counts declared footprints only.

use core::{fmt, time::Duration};
use std::{collections::BTreeSet, io::Write};

use futures_util::future::LocalBoxFuture;
use shep_client::dogs::Stop;
use tokio::time::{Instant, MissedTickBehavior, interval};

use crate::{
    config::MeterConfig,
    metrics::{self, Channel},
    survey::probe::NvidiaSmi,
};

mod kasa;
mod parts;

use kasa::{Kasa, Tcp};
#[cfg(test)]
pub(crate) use parts::Host;
use parts::{Parts, Sensors};

/// Where a reading was taken
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    /// A plug's energy meter: everything the host draws from the wall.
    Wall,
    /// The GPU and the CPU package's counters: no RAM, board, fans or power supply loss.
    Parts,
}

impl Source {
    /// The `meter` label's value.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Wall => "wall",
            Self::Parts => "parts",
        }
    }
}

/// The host's power at one moment
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Reading {
    /// Watts drawn.
    pub watts: f64,
    /// What measured them.
    pub source: Source,
}

/// Why a read gave no reading
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReadError {
    /// The plug refused the connection, or dropped it.
    Unreachable,
    /// The plug did not answer within its limit.
    TimedOut,
    /// The plug answered something that is not an energy reading, or an error code.
    Unreadable,
    /// The strip has no outlet with the configured number.
    NoOutlet,
    /// `nvidia-smi` is missing, failed, hung, or is still running for an earlier read.
    NoGpu,
    /// The CPU package's energy counter cannot be read, which Linux allows only root by default.
    NoCounter,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unreachable => "the plug refused or dropped the connection",
            Self::TimedOut => "the plug did not answer in time",
            Self::Unreadable => "the plug's answer is not an energy reading",
            Self::NoOutlet => "the strip has no such outlet",
            Self::NoGpu => "nvidia-smi gave no power draw",
            Self::NoCounter => "the CPU package's energy counter cannot be read",
        })
    }
}

impl core::error::Error for ReadError {}

/// What the dog reads the host's power through
///
/// Boxed futures so it can sit behind `dyn`.
pub(crate) trait Meter: fmt::Debug {
    /// The host's power now.
    fn read(&self) -> LocalBoxFuture<'_, Result<Reading, ReadError>>;
}

/// The meter `settings` names: the plug, or the parts, whose GPU read goes through `smi`
///
/// `smi` is a clone of the survey's, so the two share its one-at-a-time guard.
pub(crate) fn from_config(settings: &MeterConfig, smi: NvidiaSmi) -> Box<dyn Meter> {
    match &settings.kasa {
        Some(host) => Box::new(Kasa::new(
            Box::new(Tcp::new(host.clone(), kasa::PORT)),
            settings.outlet,
            kasa::LIMIT,
        )),
        None => Box::new(Parts::new(Box::new(Sensors::new(smi)))),
    }
}

/// The energy a meter's good readings add up to
#[derive(Debug, Default)]
struct Energy {
    last: Option<Instant>,
    joules: f64,
}

impl Energy {
    /// The total after a good reading at `now`: watts times the seconds since the last one
    ///
    /// The first reading adds nothing, having no earlier one to span from.
    fn add(&mut self, now: Instant, watts: f64) -> f64 {
        if let Some(last) = self.last {
            self.joules += watts * now.saturating_duration_since(last).as_secs_f64();
        }
        self.last = Some(now);
        self.joules
    }
}

/// Reads `meter` every `every` and sends the power and the energy total, until a stop is requested
///
/// A read that fails is skipped and its reason logged to `log` once, until a read succeeds.
/// Returns at once for a dog with no channel.
pub(crate) async fn report<C: Channel>(
    meter: &dyn Meter,
    every: Duration,
    channel: &C,
    mut stop: Stop,
    mut log: impl Write,
) {
    if !channel.is_active() {
        return;
    }
    let mut ticks = interval(every);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut energy = Energy::default();
    let mut logged = BTreeSet::new();
    loop {
        tokio::select! {
            biased;
            () = stop.wait() => return,
            _ = ticks.tick() => {}
        }
        let read = tokio::select! {
            biased;
            () = stop.wait() => return,
            read = meter.read() => read,
        };
        match read {
            Ok(reading) => {
                logged.clear();
                let joules = energy.add(Instant::now(), reading.watts);
                for sample in &metrics::power(&reading, joules) {
                    channel.send(sample);
                }
            }
            Err(reason) => {
                if logged.insert(reason) {
                    let _ = writeln!(log, "paddock: the host's power goes unread: {reason}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
