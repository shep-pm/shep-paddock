//! One survey's I/O: the flock, what each ollama has loaded and from which blob, and the GPU.
//!
//! The engine runs one at a time, every [`SURVEY_EVERY`], and hands the [`Reading`] to
//! [`Engine::surveyed`](super::state::Engine::surveyed). Nothing in a reading is admitted
//! against: admission counts declared footprints only (ADR 0002).

use std::{collections::BTreeMap, rc::Rc, sync::Arc, time::Duration};

use shep_client::shep_core::protocol::ProcessInfo;
use tokio::time::Instant;

use crate::{
    backend::Backends,
    config::{Config, tagged},
    discover,
    shepherd::Shepherd,
    survey::{
        gpu::{self, GpuParseError, GpuReading},
        probe::HostProbe,
    },
};

/// How often the dog surveys the host, from the spec.
pub(crate) const SURVEY_EVERY: Duration = Duration::from_secs(30);

/// How the engine surveys the host
#[derive(Debug, Clone)]
pub(crate) struct Survey {
    /// What reads `nvidia-smi` and a process's arguments.
    pub host: Rc<dyn HostProbe>,
    /// How long from one survey's start to the next.
    pub every: Duration,
}

/// Each ollama model's blob, keyed by its server's url and its tagged name, with the manifest
/// digest it was read for
pub(super) type Blobs = BTreeMap<(String, String), (Option<String>, String)>;

/// What one survey read
///
/// Derives nothing: it holds ollama urls and command lines, and nothing prints it.
pub(super) struct Reading {
    /// When the survey began, so a reading older than a job's outcome is not read against it.
    pub asked: Instant,
    /// The flock with each sheep's tree and memory, `None` when the shepherd did not describe it.
    pub flock: Option<Vec<ProcessInfo>>,
    /// The blob of each model an ollama that answered lists.
    pub blobs: Blobs,
    /// What `nvidia-smi` printed, read, `None` without it or when it could not be read.
    pub gpu: Option<GpuReading>,
    /// Why `nvidia-smi`'s output could not be read, when it could not.
    pub unreadable: Option<GpuParseError>,
    /// Each GPU process's arguments, by pid.
    pub cmdlines: BTreeMap<u32, Vec<String>>,
}

impl Reading {
    /// A reading begun at `asked` that found nothing
    #[cfg(test)]
    pub(super) fn empty(asked: Instant) -> Reading {
        Reading {
            asked,
            flock: None,
            blobs: Blobs::new(),
            gpu: None,
            unreadable: None,
            cmdlines: BTreeMap::new(),
        }
    }
}

/// Reads the host once
///
/// A blob comes from `known` while its model's manifest digest is the one it was read for,
/// and from `/api/show` otherwise. An ollama that does not answer is skipped unlogged:
/// discovery logged it at start, and the next survey asks again.
pub(super) async fn read<S: Shepherd>(
    backends: &Backends<S>,
    host: Rc<dyn HostProbe>,
    config: Arc<Config>,
    known: Blobs,
) -> Reading {
    let asked = Instant::now();
    // shep answers `Describe` of an empty flock with an error, so any error is no flock.
    let flock = backends.shepherd().describe_all().await.ok();
    let mut blobs = Blobs::new();
    for url in config.ollamas.keys() {
        let key = discover::ollama_key(&config, url);
        let Ok(listed) = backends.ollama_loaded(url, key).await else {
            continue;
        };
        for loaded in listed {
            let at = (url.clone(), tagged(&loaded.name));
            let cached = known
                .get(&at)
                .filter(|(manifest, _)| *manifest == loaded.digest)
                .cloned();
            let found = match cached {
                Some(entry) => Some(entry),
                None => backends
                    .ollama_blob(url, &loaded.name, key)
                    .await
                    .ok()
                    .flatten()
                    .map(|blob| (loaded.digest, blob)),
            };
            if let Some(entry) = found {
                blobs.insert(at, entry);
            }
        }
    }
    let (gpu, unreadable) = match host
        .gpu()
        .await
        .map(|text| gpu::reading(&text.totals, &text.apps))
    {
        Some(Ok(reading)) => (Some(reading), None),
        Some(Err(err)) => (None, Some(err)),
        None => (None, None),
    };
    let mut cmdlines = BTreeMap::new();
    for app in gpu.iter().flat_map(|gpu| &gpu.apps) {
        if let Some(args) = host.cmdline(app.pid).await {
            cmdlines.insert(app.pid, args);
        }
    }
    Reading {
        asked,
        flock,
        blobs,
        gpu,
        unreadable,
        cmdlines,
    }
}
