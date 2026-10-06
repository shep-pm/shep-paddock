//! What each loaded model holds, measured, and the GPU memory nothing the dog can name holds
//!
//! A sheep's model is measured by its sheep's process tree: the GPU memory its pids hold, and the
//! RAM shep reports for the tree. An ollama model is measured by the runner whose arguments name
//! its model blob, and its RAM is not measured. Unaccounted is the GPU memory in use that no
//! tracked model's sheep and no ollama runner holds.
//!
//! The figures are reported, never admitted against: admission counts declared footprints only
//! (ADR 0002).

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use shep_client::shep_core::protocol::ProcessInfo;

use crate::{
    config::ModelName,
    footprint::{Footprint, Vram},
};

pub(crate) mod drift;
pub(crate) mod gpu;

#[cfg(test)]
mod tests;

use gpu::GpuReading;

/// Where a tracked model runs, as the survey needs it
///
/// No url: the engine resolves an ollama model's blob, so nothing here can carry a credential
/// into a `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code, reason = "the engine's survey builds it"))]
pub(crate) enum Where {
    /// On the sheep of this name.
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
}

impl fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inputs")
            .field("tracked", &self.tracked)
            .field("flock", &self.flock)
            .field("blobs", &self.blobs)
            .field("gpu", &self.gpu)
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
    /// GPU memory in use that nothing tracked holds, in bytes, `None` when it cannot be known.
    pub unaccounted_vram: Option<u64>,
}

/// Each tracked model's measured figures, and the GPU memory no tracked model or runner holds
///
/// An ollama model whose blob `blobs` does not list is unmeasured: its runner is unaccounted, and
/// counting it as the model's too would count it twice.
///
/// Unaccounted is `None` without a GPU reading, and while a tracked model declares
/// `vram = "all"`: it takes whatever is free, and a podman sheep's GPU process is outside its tree.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the engine's survey is its caller")
)]
pub(crate) fn measure(inputs: &Inputs<'_>) -> Measures {
    let tree_of = |sheep: &str| -> BTreeSet<u32> {
        inputs
            .flock
            .iter()
            .filter(|row| row.name == sheep)
            .flat_map(|row| {
                row.pid
                    .into_iter()
                    .chain(row.lambs.iter().flatten().map(|lamb| lamb.pid))
            })
            .collect()
    };
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
    let models = inputs
        .tracked
        .iter()
        .map(|tracked| {
            let measured = match &tracked.on {
                Where::Sheep(sheep) => Measured {
                    vram: vram_of(&tree_of(sheep)),
                    ram: inputs
                        .flock
                        .iter()
                        .find(|row| row.name == *sheep)
                        .and_then(|row| row.memory_bytes),
                },
                Where::Ollama { blob } => Measured {
                    vram: blob
                        .as_deref()
                        .filter(|blob| inputs.blobs.iter().any(|listed| listed == blob))
                        .map(runners)
                        .and_then(|pids| vram_of(&pids)),
                    ram: None,
                },
            };
            (tracked.model.clone(), measured)
        })
        .collect();
    let all_loaded = inputs
        .tracked
        .iter()
        .any(|tracked| tracked.declared.vram == Vram::All);
    let unaccounted_vram = inputs.gpu.filter(|_| !all_loaded).map(|gpu| {
        // Only a tracked model's sheep and ollama's runners own memory (Spec readings 2).
        let mut owned: BTreeSet<u32> = inputs
            .tracked
            .iter()
            .filter_map(|tracked| match &tracked.on {
                Where::Sheep(sheep) => Some(tree_of(sheep)),
                Where::Ollama { .. } => None,
            })
            .flatten()
            .collect();
        for blob in inputs.blobs {
            owned.extend(runners(blob));
        }
        let attributed = gpu
            .apps
            .iter()
            .filter(|app| owned.contains(&app.pid))
            .fold(0_u64, |sum, app| sum.saturating_add(app.used));
        gpu.used.saturating_sub(attributed)
    });
    Measures {
        models,
        unaccounted_vram,
    }
}

/// Whether `args` run `blob`: one of them ends in `/blobs/sha256-<blob>`
fn loads(args: &[String], blob: &str) -> bool {
    let wanted = format!("/blobs/sha256-{blob}");
    !blob.is_empty() && args.iter().any(|arg| arg.ends_with(&wanted))
}
