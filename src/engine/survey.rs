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
    backend::{Backends, OllamaLoaded},
    config::{Config, tagged},
    discover,
    shepherd::Shepherd,
    survey::{
        ContainerRead,
        gpu::{self, GpuParseError, GpuReading},
        podman::Container,
        probe::{Args, HostProbe},
    },
};

/// How often the dog surveys the host, from the spec.
pub(crate) const SURVEY_EVERY: Duration = Duration::from_secs(30);
// A process tree deeper than this is a fork loop, or pids reused mid-walk.
const PARENT_STEPS: usize = 64;

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
    /// What each ollama that answered `/api/ps` lists, by its url.
    pub ollama: Vec<(String, Vec<OllamaLoaded>)>,
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
    /// What each container the config names holds, by name, running ones only; `None` when one
    /// could not be read.
    pub containers: Option<BTreeMap<String, ContainerRead>>,
    /// Why a container could not be read, when one could not.
    pub podman: Option<String>,
    /// The parent of each GPU process, and of its ancestors, up to a bare lease's pid.
    pub parents: BTreeMap<u32, u32>,
}

impl Reading {
    /// A reading begun at `asked` that found nothing
    #[cfg(test)]
    pub(super) fn empty(asked: Instant) -> Reading {
        Reading {
            asked,
            flock: None,
            blobs: Blobs::new(),
            ollama: Vec::new(),
            unanswered: BTreeSet::new(),
            gpu: None,
            unreadable: None,
            cmdlines: BTreeMap::new(),
            unread_cmdlines: BTreeSet::new(),
            containers: Some(BTreeMap::new()),
            podman: None,
            parents: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reading")
            .field("flock", &self.flock.as_ref().map(Vec::len))
            .field("blobs", &self.blobs.len())
            .field("ollama", &self.ollama.len())
            .field("unanswered", &self.unanswered.len())
            .field("gpu", &self.gpu)
            .field("unreadable", &self.unreadable)
            .field("cmdlines", &self.cmdlines.len())
            .field("unread_cmdlines", &self.unread_cmdlines.len())
            .field("containers", &self.containers.as_ref().map(BTreeMap::len))
            .field("podman", &self.podman)
            .field("parents", &self.parents.len())
            .finish_non_exhaustive()
    }
}

/// Reads the host once
///
/// A blob comes from `known` while `/api/ps` gives its model's manifest digest and that is the
/// one it was read for, and from `/api/show` otherwise. An ollama that does not answer keeps its cached blobs and is
/// named in [`Reading::unanswered`], unlogged: discovery logged it at start.
pub(super) async fn read<S: Shepherd>(
    backends: &Backends<S>,
    host: Rc<dyn HostProbe>,
    config: Arc<Config>,
    known: Blobs,
    bare_pids: BTreeSet<u32>,
) -> Reading {
    let asked = Instant::now();
    // `None` is an error, which shep also answers for an empty flock.
    let flock = backends.shepherd().describe_all().await.ok();
    let mut blobs = Blobs::new();
    let mut ollama = Vec::new();
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
        for loaded in &listed {
            let at = (url.clone(), tagged(&loaded.name));
            // With no digest, nothing would show a pull that changed the blob.
            let cached = known
                .get(&at)
                .filter(|(manifest, _)| loaded.digest.is_some() && *manifest == loaded.digest)
                .cloned();
            let found = match cached {
                Some(entry) => Some(entry),
                None => backends
                    .ollama_blob(url, &loaded.name, key)
                    .await
                    .ok()
                    .flatten()
                    .map(|blob| (loaded.digest.clone(), blob)),
            };
            if let Some(entry) = found {
                blobs.insert(at, entry);
            }
        }
        ollama.push((url.clone(), listed));
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
    let mut parents = BTreeMap::new();
    if !bare_pids.is_empty() {
        for app in gpu.iter().flat_map(|gpu| &gpu.apps) {
            walk_up(&*host, app.pid, &bare_pids, &mut parents).await;
        }
    }
    let (containers, podman) = read_containers(&*host, &config).await;
    Reading {
        asked,
        flock,
        blobs,
        ollama,
        unanswered,
        gpu,
        unreadable,
        cmdlines,
        unread_cmdlines,
        containers,
        podman,
        parents,
    }
}

/// Reads the parent of `pid`, and of each ancestor, into `parents`, until one is a bare lease's
/// pid, pid 1, unreadable, already read, or [`PARENT_STEPS`] up
async fn walk_up(
    host: &dyn HostProbe,
    pid: u32,
    roots: &BTreeSet<u32>,
    parents: &mut BTreeMap<u32, u32>,
) {
    let mut at = pid;
    for _ in 0..PARENT_STEPS {
        if roots.contains(&at) || at <= 1 || parents.contains_key(&at) {
            return;
        }
        let Some(parent) = host.parent(at).await else {
            return;
        };
        parents.insert(at, parent);
        at = parent;
    }
}

/// What each container the config names holds, or why one could not be read
///
/// One container podman cannot be asked about, or whose cgroup or memory cannot be read, makes the
/// whole reading unknown: a part read would be taken as the whole.
async fn read_containers(
    host: &dyn HostProbe,
    config: &Config,
) -> (Option<BTreeMap<String, ContainerRead>>, Option<String>) {
    let names: BTreeSet<&str> = config
        .models
        .values()
        .filter_map(|model| model.container.as_deref())
        .collect();
    let mut read = BTreeMap::new();
    for name in names {
        match host.container(name).await {
            Container::Running(pid) => {
                let Some(pids) = host.cgroup_pids(pid).await else {
                    return (
                        None,
                        Some(format!("the cgroup of {name:?} could not be read")),
                    );
                };
                let mut ram = 0_u64;
                for pid in &pids {
                    let Some(rss) = host.rss(*pid).await else {
                        return (
                            None,
                            Some(format!(
                                "the memory of {name:?}'s process {pid} could not be read"
                            )),
                        );
                    };
                    ram = ram.saturating_add(rss);
                }
                read.insert(name.to_owned(), ContainerRead { pids, ram });
            }
            Container::Stopped => {}
            Container::Unreadable(why) => return (None, Some(why)),
        }
    }
    (Some(read), None)
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
            ollama: vec![(SECRET_URL.to_owned(), Vec::new())],
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
            "Reading { flock: None, blobs: 1, ollama: 1, unanswered: 1, gpu: None, \
             unreadable: None, cmdlines: 1, unread_cmdlines: 0, containers: Some(0), podman: None, \
             parents: 0, .. }"
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
        let _ = engine.surveyed(holding_secrets(), |_| false);

        let shown = format!("{engine:?}");
        assert!(!engine.blobs().is_empty(), "the cache holds the url");
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
