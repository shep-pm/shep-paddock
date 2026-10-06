//! One survey's I/O: the flock, what each ollama has loaded and from which blob, and the GPU.
//!
//! The engine runs one at a time, every [`SURVEY_EVERY`], and hands the [`Reading`] to
//! [`Engine::surveyed`](super::state::Engine::surveyed). Nothing in a reading is admitted
//! against: admission counts declared footprints only (ADR 0002).

use core::fmt;
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use shep_client::shep_core::protocol::ProcessInfo;
use tokio::time::Instant;

use crate::{
    backend::Backends,
    config::{Config, tagged},
    discover,
    shepherd::Shepherd,
    survey::{
        gpu::{self, GpuParseError, GpuReading},
        probe::{Args, HostProbe},
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
/// `Debug` prints counts: an ollama url or a command line can carry a credential.
pub(super) struct Reading {
    /// When the survey began, so a reading older than a job's outcome is not read against it.
    pub asked: Instant,
    /// The flock with each sheep's tree and memory, `None` when the shepherd did not describe it.
    pub flock: Option<Vec<ProcessInfo>>,
    /// The blob of each model an ollama that answered lists, and the cached blobs of one that
    /// did not.
    pub blobs: Blobs,
    /// The url of each ollama that did not answer `/api/ps`.
    pub unanswered: BTreeSet<String>,
    /// What `nvidia-smi` printed, read, `None` without it or when it could not be read.
    pub gpu: Option<GpuReading>,
    /// Why `nvidia-smi`'s output could not be read, when it could not.
    pub unreadable: Option<GpuParseError>,
    /// Each GPU process's arguments, by pid.
    pub cmdlines: BTreeMap<u32, Vec<String>>,
    /// The GPU processes whose arguments could not be read, which may be anything.
    pub unread_cmdlines: BTreeSet<u32>,
}

impl Reading {
    /// A reading begun at `asked` that found nothing
    #[cfg(test)]
    pub(super) fn empty(asked: Instant) -> Reading {
        Reading {
            asked,
            flock: None,
            blobs: Blobs::new(),
            unanswered: BTreeSet::new(),
            gpu: None,
            unreadable: None,
            cmdlines: BTreeMap::new(),
            unread_cmdlines: BTreeSet::new(),
        }
    }
}

impl fmt::Debug for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reading")
            .field("flock", &self.flock.as_ref().map(Vec::len))
            .field("blobs", &self.blobs.len())
            .field("unanswered", &self.unanswered.len())
            .field("gpu", &self.gpu)
            .field("unreadable", &self.unreadable)
            .field("cmdlines", &self.cmdlines.len())
            .field("unread_cmdlines", &self.unread_cmdlines.len())
            .finish_non_exhaustive()
    }
}

/// Reads the host once
///
/// A blob comes from `known` while its model's manifest digest is the one it was read for,
/// and from `/api/show` otherwise. An ollama that does not answer keeps its cached blobs and is
/// named in [`Reading::unanswered`], unlogged: discovery logged it at start.
pub(super) async fn read<S: Shepherd>(
    backends: &Backends<S>,
    host: Rc<dyn HostProbe>,
    config: Arc<Config>,
    known: Blobs,
) -> Reading {
    let asked = Instant::now();
    // `None` is an error, which shep also answers for an empty flock.
    let flock = backends.shepherd().describe_all().await.ok();
    let mut blobs = Blobs::new();
    let mut unanswered = BTreeSet::new();
    for url in config.ollamas.keys() {
        let key = discover::ollama_key(&config, url);
        let Ok(listed) = backends.ollama_loaded(url, key).await else {
            // What ran from these blobs may run on, so they are kept for the next survey.
            let kept = known.iter().filter(|((at, _), _)| at == url);
            blobs.extend(kept.map(|(at, entry)| (at.clone(), entry.clone())));
            unanswered.insert(url.clone());
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
    let mut unread_cmdlines = BTreeSet::new();
    for app in gpu.iter().flat_map(|gpu| &gpu.apps) {
        match host.cmdline(app.pid).await {
            Args::Read(args) => {
                cmdlines.insert(app.pid, args);
            }
            Args::Gone => {}
            Args::Unknown => {
                unread_cmdlines.insert(app.pid);
            }
        }
    }
    Reading {
        asked,
        flock,
        blobs,
        unanswered,
        gpu,
        unreadable,
        cmdlines,
        unread_cmdlines,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tokio::{sync::mpsc, time::Instant};

    use super::{Blobs, Reading};
    use crate::{
        engine::{Clock, state::Engine},
        test_support::config,
    };

    const SECRET_URL: &str = "http://paddock:hunter2@127.0.0.1:11434";

    fn holding_secrets() -> Reading {
        let at = (SECRET_URL.to_owned(), "qwen3.8:27b".to_owned());
        Reading {
            blobs: Blobs::from([(at, (None, "f5f1".to_owned()))]),
            unanswered: BTreeSet::from([SECRET_URL.to_owned()]),
            cmdlines: BTreeMap::from([(7, vec!["--api-key=hunter2".to_owned()])]),
            ..Reading::empty(Instant::now())
        }
    }

    /// A derived `Debug` would print the ollama urls and the arguments, either of which can
    /// carry a credential.
    #[tokio::test(start_paused = true)]
    async fn a_readings_debug_prints_counts_not_urls_or_arguments() {
        assert_eq!(
            format!("{:?}", holding_secrets()),
            "Reading { flock: None, blobs: 1, unanswered: 1, gpu: None, unreadable: None, \
             cmdlines: 1, unread_cmdlines: 0, .. }"
        );
    }

    /// The engine keeps the blob cache, keyed by raw ollama urls, between surveys.
    #[tokio::test(start_paused = true)]
    async fn the_engines_debug_leaves_out_the_blob_cache() {
        let (notify, _) = mpsc::unbounded_channel();
        let mut engine = Engine::new(
            config("[host]\nvram = \"1G\"\nram = \"1G\"\n"),
            Clock::new(),
            notify,
        );
        let _ = engine.surveyed(holding_secrets());

        let shown = format!("{engine:?}");
        assert!(!engine.blobs().is_empty(), "the cache holds the url");
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
