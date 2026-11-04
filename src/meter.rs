//! The host's power, read on a timer, given to the engine to count the energy its leases use, and
//! sent to the metrics dog.
//!
//! Two sources. A Kasa plug's energy meter reads what the wall supplies, losses included. With no
//! plug, the GPU's draw and the CPU package's power are summed, which leaves out RAM, the board,
//! the fans and the power supply's loss, so it is reported under its own `meter` value. Admission
//! never reads either: it counts declared footprints only.

use core::{fmt, time::Duration};
use std::{collections::BTreeSet, io::Write};

use futures_util::future::LocalBoxFuture;
use shep_client::dogs::Stop;
use tokio::{
    sync::watch,
    time::{Instant, MissedTickBehavior, interval},
};

use crate::{
    config::MeterConfig,
    engine::{EngineHandle, Metered},
    metrics::{self, Channel, Totals},
    price::Price,
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

/// Reads `meter` every `every`, gives each reading to the engine to split among the leases in
/// use, and sends the power and the energy totals until a stop is requested
///
/// A read that fails is skipped and its reason logged to `log` once, until a read succeeds. The
/// readings are taken whether or not `channel` reaches the metrics dog, since the leases' energy
/// depends on them; only sending the samples does.
pub(crate) async fn report<C: Channel>(
    meter: &dyn Meter,
    every: Duration,
    channel: &C,
    engine: &EngineHandle,
    price: watch::Receiver<Option<Price>>,
    mut stop: Stop,
    mut log: impl Write,
) {
    let mut ticks = interval(every);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut totals = Totals::default();
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
                let metered = Metered {
                    reading,
                    at: Instant::now(),
                    usd_per_kwh: price.borrow().as_ref().map(|price| price.usd_per_kwh),
                };
                let shares = tokio::select! {
                    biased;
                    () = stop.wait() => return,
                    shares = engine.metered(metered) => shares,
                };
                totals.add(&shares);
                if channel.is_active() {
                    for sample in &metrics::power(&reading, &totals) {
                        channel.send(sample);
                    }
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
