//! What `nvidia-smi` prints, read.

use core::fmt;

const MIB: u64 = 1 << 20;

/// One process holding GPU memory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GpuApp {
    /// Its pid.
    pub pid: u32,
    /// The GPU memory it holds, in bytes.
    pub used: u64,
}

/// The GPU memory in use, across every GPU, and who holds it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GpuReading {
    /// In use, in bytes.
    pub used: u64,
    /// On the cards, in bytes.
    pub total: u64,
    /// The processes holding some, in the order `nvidia-smi` listed them.
    pub apps: Vec<GpuApp>,
}

/// Why `nvidia-smi`'s output could not be read
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GpuParseError {
    /// nvidia-smi listed no GPU.
    NoGpu,
    /// A line that is not the fields asked for, in MiB.
    Line {
        /// The line as printed.
        line: String,
    },
}

impl fmt::Display for GpuParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGpu => f.write_str("nvidia-smi listed no GPU"),
            Self::Line { line } => {
                write!(f, "nvidia-smi printed a line the dog cannot read: {line}")
            }
        }
    }
}

impl core::error::Error for GpuParseError {}

/// A `<n> MiB` field as bytes
fn mib(field: &str) -> Option<u64> {
    field
        .trim()
        .strip_suffix(" MiB")?
        .parse::<u64>()
        .ok()?
        .checked_mul(MIB)
}

/// What `nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader` and
/// `--query-compute-apps=pid,process_name,used_memory --format=csv,noheader` printed, read
///
/// Several GPUs sum. A process whose memory is not a MiB figure is left out:
/// `nvidia-smi` writes `[N/A]` there when it cannot read the process.
///
/// # Errors
/// [`GpuParseError::NoGpu`] when the totals list no GPU, and [`GpuParseError::Line`] for a
/// line that is not the fields asked for in MiB.
pub(crate) fn reading(totals: &str, apps: &str) -> Result<GpuReading, GpuParseError> {
    let lines = |text: &str| {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let bad = |line: &str| GpuParseError::Line {
        line: line.to_owned(),
    };
    let gpus = lines(totals);
    if gpus.is_empty() {
        return Err(GpuParseError::NoGpu);
    }
    let (mut used, mut total) = (0_u64, 0_u64);
    for line in &gpus {
        let (u, t) = line.split_once(',').ok_or_else(|| bad(line))?;
        used = used.saturating_add(mib(u).ok_or_else(|| bad(line))?);
        total = total.saturating_add(mib(t).ok_or_else(|| bad(line))?);
    }
    let mut found = Vec::new();
    for line in &lines(apps) {
        let (pid, rest) = line.split_once(',').ok_or_else(|| bad(line))?;
        let (_name, memory) = rest.rsplit_once(',').ok_or_else(|| bad(line))?;
        let pid = pid.trim().parse().map_err(|_| bad(line))?;
        if let Some(used) = mib(memory) {
            found.push(GpuApp { pid, used });
        }
    }
    Ok(GpuReading {
        used,
        total,
        apps: found,
    })
}
