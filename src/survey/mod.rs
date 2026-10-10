//! What each loaded model holds, measured, and the GPU memory nothing the dog can name holds
//!
//! A sheep's model is measured by its sheep's process tree: the GPU memory its pids hold, and the
//! RAM shep reports for the tree. An ollama model is measured by the runner whose arguments name
//! its model blob, and its RAM is not measured. Unaccounted is the GPU memory in use that no
//! tracked model's sheep and no ollama runner holds. A sheep model in a podman container is
//! measured over its container's processes too. A bare lease's job is measured by the GPU
//! processes whose parent chain reaches its pid.
//!
//! The figures are reported, never admitted against: admission counts declared footprints only
//! (ADR 0002).

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use shep_client::shep_core::protocol::ProcessInfo;

use crate::{
    book::LeaseId,
    config::ModelName,
    footprint::{Footprint, Vram},
};

pub(crate) mod drift;
pub(crate) mod gpu;
pub(crate) mod podman;
pub(crate) mod probe;
pub(crate) mod procfs;

#[cfg(test)]
mod tests;

use gpu::GpuReading;

/// Where a tracked model runs, as the survey needs it
///
/// No url: the engine resolves an ollama model's blob, so nothing here can carry a credential
/// into a `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Where {
    /// On the sheep of this name, whose whole tree is this model's
    ///
    /// Models on one sheep exclude each other (`Config::excluded`), since a sheep runs one
    /// process. A model that left the sheep since the survey began may be listed beside the
    /// one now there. Both get the tree, and the engine drops the leaver's figures.
    Sheep(String),
    /// On ollama, run from this model blob when it is known.
    Ollama {
        /// The blob's sha256, in hex.
        blob: Option<String>,
    },
}

/// A model the dog tracks as loaded
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tracked {
    /// The model.
    pub model: ModelName,
    /// Where it runs.
    pub on: Where,
    /// The footprint it counts at.
    pub declared: Footprint,
    /// What its container held, for a sheep model naming one that runs.
    pub container: Option<ContainerRead>,
}

/// What a model's container held, as one survey read it
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContainerRead {
    /// The pids in its cgroup and those below it.
    pub pids: BTreeSet<u32>,
    /// Their resident memory, summed, in bytes.
    pub ram: u64,
}

/// A bare lease, as the survey measures it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BareJob {
    /// The lease.
    pub lease: LeaseId,
    /// The process its job runs under, when the dog may read it.
    pub pid: Option<u32>,
    /// What it declares.
    pub declared: Footprint,
}

impl BareJob {
    /// The pid its job's GPU processes are walked up to
    ///
    /// None for 0 or 1: every process descends from init.
    pub fn root(&self) -> Option<u32> {
        self.pid.filter(|pid| *pid > 1)
    }
}

/// One survey's readings, as [`measure`] takes them
///
/// `Debug` leaves out `cmdlines`: a command line can carry a key.
pub(crate) struct Inputs<'a> {
    /// The models to measure.
    pub tracked: &'a [Tracked],
    /// The flock, with each sheep's lambs and memory.
    pub flock: &'a [ProcessInfo],
    /// The model blob of every model ollama has loaded.
    pub blobs: &'a [String],
    /// What `nvidia-smi` printed, `None` without it.
    pub gpu: Option<&'a GpuReading>,
    /// Each GPU process's arguments, by pid.
    pub cmdlines: &'a BTreeMap<u32, Vec<String>>,
    /// Every bare lease the status lists.
    pub bare: &'a [BareJob],
    /// The parent of each GPU process, and of its ancestors, as far as the survey read them.
    pub parents: &'a BTreeMap<u32, u32>,
}

impl fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inputs")
            .field("tracked", &self.tracked)
            .field("flock", &self.flock)
            .field("blobs", &self.blobs)
            .field("gpu", &self.gpu)
            .field("bare", &self.bare)
            .field("parents", &self.parents.len())
            .finish_non_exhaustive()
    }
}

/// What one model was measured holding
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Measured {
    /// VRAM in bytes, `None` when unmeasured.
    pub vram: Option<u64>,
    /// RAM in bytes, `None` when unmeasured.
    pub ram: Option<u64>,
}

/// What one survey measured
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Measures {
    /// Each tracked model's figures.
    pub models: BTreeMap<ModelName, Measured>,
    /// Each bare lease's figures.
    pub leases: BTreeMap<LeaseId, Measured>,
    /// GPU memory in use that nothing tracked holds, in bytes, `None` when it cannot be known.
    pub unaccounted_vram: Option<u64>,
}

/// Each tracked model's measured figures, and the GPU memory no tracked model or runner holds
///
/// An ollama model whose blob `blobs` does not list is unmeasured: its runner is unaccounted, and
/// counting it as the model's too would count it twice. One whose blob `blobs` lists more than
/// once is unmeasured too, and its runners are ollama's.
///
/// Unaccounted is `None` without a GPU reading, and while a tracked model or a bare lease declares
/// `vram = "all"`: it takes whatever is free.
pub(crate) fn measure(inputs: &Inputs<'_>) -> Measures {
    let runners = |blob: &str| -> BTreeSet<u32> {
        inputs
            .cmdlines
            .iter()
            .filter(|(_, args)| loads(args, blob))
            .map(|(pid, _)| *pid)
            .collect()
    };
    let vram_of = |pids: &BTreeSet<u32>| -> Option<u64> {
        let apps: Vec<_> = inputs
            .gpu?
            .apps
            .iter()
            .filter(|app| pids.contains(&app.pid))
            .collect();
        (!apps.is_empty()).then(|| {
            apps.iter()
                .fold(0_u64, |sum, app| sum.saturating_add(app.used))
        })
    };
    let job_of = |job: &BareJob| -> BTreeSet<u32> {
        let Some(root) = job.root() else {
            return BTreeSet::new();
        };
        inputs
            .gpu
            .iter()
            .flat_map(|gpu| &gpu.apps)
            .map(|app| app.pid)
            .filter(|pid| descends(inputs.parents, *pid, root))
            .collect()
    };
    let models = inputs
        .tracked
        .iter()
        .map(|tracked| {
            let measured = match &tracked.on {
                Where::Sheep(sheep) => {
                    let tree_ram = inputs
                        .flock
                        .iter()
                        .find(|row| row.name == *sheep)
                        .and_then(|row| row.memory_bytes);
                    let ram = match (tree_ram, &tracked.container) {
                        (Some(tree), Some(read)) => Some(tree.saturating_add(read.ram)),
                        (tree, None) => tree,
                        (None, Some(read)) => Some(read.ram),
                    };
                    Measured {
                        vram: vram_of(&pids_of(inputs.flock, tracked, sheep)),
                        ram,
                    }
                }
                Where::Ollama { blob } => Measured {
                    vram: blob
                        .as_deref()
                        // A blob listed twice runs twice, and nothing says which runner is whose.
                        .filter(|blob| {
                            inputs.blobs.iter().filter(|listed| listed == blob).count() == 1
                        })
                        .map(runners)
                        .and_then(|pids| vram_of(&pids)),
                    ram: None,
                },
            };
            (tracked.model.clone(), measured)
        })
        .collect();
    let leases = inputs
        .bare
        .iter()
        .map(|job| {
            let measured = Measured {
                vram: vram_of(&job_of(job)),
                ram: None,
            };
            (job.lease, measured)
        })
        .collect();
    let all = inputs
        .tracked
        .iter()
        .map(|tracked| tracked.declared.vram)
        .chain(inputs.bare.iter().map(|job| job.declared.vram))
        .any(|vram| vram == Vram::All);
    let unaccounted_vram = inputs.gpu.filter(|_| !all).map(|gpu| {
        // Only a tracked model's sheep and container, ollama's runners and bare jobs own memory.
        let mut owned: BTreeSet<u32> = inputs
            .tracked
            .iter()
            .filter_map(|tracked| match &tracked.on {
                Where::Sheep(sheep) => Some(pids_of(inputs.flock, tracked, sheep)),
                Where::Ollama { .. } => None,
            })
            .flatten()
            .collect();
        for blob in inputs.blobs {
            owned.extend(runners(blob));
        }
        for job in inputs.bare {
            owned.extend(job_of(job));
        }
        let attributed = gpu
            .apps
            .iter()
            .filter(|app| owned.contains(&app.pid))
            .fold(0_u64, |sum, app| sum.saturating_add(app.used));
        // A bare job with no pid holds its declared VRAM where this survey cannot see.
        let unseen =
            inputs
                .bare
                .iter()
                .filter(|job| job.root().is_none())
                .fold(0_u64, |sum, job| match job.declared.vram {
                    Vram::Bytes(bytes) => sum.saturating_add(bytes),
                    Vram::None | Vram::All => sum,
                });
        gpu.used.saturating_sub(attributed).saturating_sub(unseen)
    });
    Measures {
        models,
        leases,
        unaccounted_vram,
    }
}

/// Whether `pid` is `root` or descends from it, as far as `parents` reads; a loop ends the walk
fn descends(parents: &BTreeMap<u32, u32>, pid: u32, root: u32) -> bool {
    let mut at = pid;
    for _ in 0..=parents.len() {
        if at == root {
            return true;
        }
        match parents.get(&at) {
            Some(up) => at = *up,
            None => return false,
        }
    }
    false
}

/// A sheep model's processes: its sheep's tree, and its container's when it runs in one
pub(crate) fn pids_of(flock: &[ProcessInfo], tracked: &Tracked, sheep: &str) -> BTreeSet<u32> {
    let mut pids = tree(flock, sheep);
    pids.extend(
        tracked
            .container
            .iter()
            .flat_map(|read| read.pids.iter().copied()),
    );
    pids
}

/// The pids of `sheep`'s process and its lambs, as `flock` lists them
pub(crate) fn tree(flock: &[ProcessInfo], sheep: &str) -> BTreeSet<u32> {
    flock
        .iter()
        .filter(|row| row.name == sheep)
        .flat_map(|row| {
            row.pid
                .into_iter()
                .chain(row.lambs.iter().flatten().map(|lamb| lamb.pid))
        })
        .collect()
}

/// Whether `args` run `blob`: one of them ends in `/blobs/sha256-<blob>`
fn loads(args: &[String], blob: &str) -> bool {
    let wanted = format!("/blobs/sha256-{blob}");
    !blob.is_empty() && args.iter().any(|arg| arg.ends_with(&wanted))
}
