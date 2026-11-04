//! The host's power from its parts: the GPU's draw from `nvidia-smi`, and the CPU package's from
//! Intel's RAPL energy counter.
//!
//! The counter counts microjoules and wraps at its range, so its power is the difference of two
//! reads over the time between them. Linux lets only root read it by default.

use core::time::Duration;

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use tokio::time::{Instant, sleep, timeout};

use super::{Meter, ReadError, Reading, Source};
use crate::survey::probe::NvidiaSmi;

/// The package the counter is read for
const RAPL: &str = "/sys/class/powercap/intel-rapl:0";
/// How long the counter is left to count between its two reads
const WINDOW: Duration = Duration::from_millis(500);
// sysfs answers in microseconds, so a read past this is stuck.
const SYSFS_TIMEOUT: Duration = Duration::from_secs(1);

/// What the power is read off
pub(crate) trait Host: core::fmt::Debug {
    /// What `nvidia-smi` prints for `power.draw`, a number of watts on each GPU's line
    fn gpu_draw(&self) -> LocalBoxFuture<'_, Option<String>>;

    /// The package's energy counter, in microjoules
    fn energy_uj(&self) -> LocalBoxFuture<'_, Option<u64>>;

    /// Where the counter wraps to zero, in microjoules
    fn energy_range_uj(&self) -> LocalBoxFuture<'_, Option<u64>>;
}

/// The host as it is: `nvidia-smi` through the survey's guard, and RAPL's files
#[derive(Debug)]
pub(crate) struct Sensors {
    smi: NvidiaSmi,
}

impl Sensors {
    /// Reads the GPU through `smi`, so it never runs beside the survey's own `nvidia-smi`
    pub(crate) fn new(smi: NvidiaSmi) -> Self {
        Self { smi }
    }
}

async fn counter(file: &'static str) -> Option<u64> {
    let read = tokio::task::spawn_blocking(move || {
        std::fs::read_to_string(format!("{RAPL}/{file}"))
            .ok()?
            .trim()
            .parse()
            .ok()
    });
    timeout(SYSFS_TIMEOUT, read).await.ok()?.ok()?
}

impl Host for Sensors {
    fn gpu_draw(&self) -> LocalBoxFuture<'_, Option<String>> {
        self.smi.power_draw().boxed_local()
    }

    fn energy_uj(&self) -> LocalBoxFuture<'_, Option<u64>> {
        counter("energy_uj").boxed_local()
    }

    fn energy_range_uj(&self) -> LocalBoxFuture<'_, Option<u64>> {
        counter("max_energy_range_uj").boxed_local()
    }
}

/// The GPU's and the CPU package's power, summed
#[derive(Debug)]
pub(crate) struct Parts {
    host: Box<dyn Host>,
}

impl Parts {
    /// Reads the power off `host`
    pub(crate) fn new(host: Box<dyn Host>) -> Self {
        Self { host }
    }

    async fn gpu(&self) -> Result<f64, ReadError> {
        let printed = self.host.gpu_draw().await.ok_or(ReadError::NoGpu)?;
        gpu_watts(&printed).ok_or(ReadError::NoGpu)
    }

    async fn cpu(&self) -> Result<f64, ReadError> {
        let range = self.host.energy_range_uj().await;
        let first = self.host.energy_uj().await;
        let started = Instant::now();
        sleep(WINDOW).await;
        let second = self.host.energy_uj().await;
        let elapsed = started.elapsed();
        match (range, first, second) {
            (Some(range), Some(first), Some(second)) => {
                package_watts(first, second, range, elapsed)
            }
            _ => None,
        }
        .ok_or(ReadError::NoCounter)
    }
}

impl Meter for Parts {
    fn read(&self) -> LocalBoxFuture<'_, Result<Reading, ReadError>> {
        async {
            let (gpu, cpu) = tokio::join!(self.gpu(), self.cpu());
            Ok(Reading {
                watts: gpu? + cpu?,
                source: Source::Parts,
            })
        }
        .boxed_local()
    }
}

/// The watts on every line of `nvidia-smi`'s `power.draw`, added, or `None` if any is not a number
///
/// A GPU the driver cannot read prints `[N/A]`, which counts as no reading rather than 0 W.
fn gpu_watts(printed: &str) -> Option<f64> {
    let mut lines = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first: f64 = lines.next()?.parse().ok()?;
    lines.try_fold(first, |sum, line| Some(sum + line.parse::<f64>().ok()?))
}

/// The power a counter that went from `first` to `second` in `elapsed` shows, if it wraps at `range`
fn package_watts(first: u64, second: u64, range: u64, elapsed: Duration) -> Option<f64> {
    let counted = if second >= first {
        second - first
    } else {
        range.checked_sub(first)? + second
    };
    let seconds = elapsed.as_secs_f64();
    (seconds > 0.0).then(|| counted as f64 / 1_000_000.0 / seconds)
}
