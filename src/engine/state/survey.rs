//! What the survey measured: each model's and bare lease's figures and drift, and the GPU memory
//! nobody holds.

use std::collections::{BTreeMap, BTreeSet};

use tokio::time::Instant;

use super::Engine;
use crate::{
    book::{LeaseId, Snapshot},
    config::{Backend, ModelName, tagged},
    engine::survey::{Blobs, Reading},
    footprint::Footprint,
    survey::{self, BareJob, ContainerRead, Inputs, Measured, Tracked, Where, drift::Read},
};

impl Engine {
    /// Counts strays and measures each model holding memory and each bare lease from one survey's
    /// reading, and returns the lines to log
    ///
    /// One line per stray found or forgotten, from the flock, each ollama that answered and each
    /// container podman described; a sheep `busy` names is never one. One when a model or bare
    /// lease starts drifting and one when it stops. One when `nvidia-smi`'s output, or the
    /// containers, turn unreadable in a new way.
    ///
    /// A model whose job reported after the survey began is not measured: the reading may be
    /// from before its load or unload. What its tree held is still its own, not unaccounted.
    /// Unaccounted is unknown while the flock, an ollama that may hold memory, the arguments of a
    /// GPU process outside every tracked sheep, or a tracked model's container went unread.
    #[must_use = "the lines are for the dog's log"]
    pub fn surveyed(&mut self, reading: Reading, busy: impl Fn(&str) -> bool) -> Vec<String> {
        let Reading {
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
        } = reading;
        let mut lines = self.sheep_strays(flock.as_deref(), asked, &busy, containers.as_ref());
        if let Some(running) = &containers {
            lines.extend(self.container_strays(running, flock.as_deref(), asked, &busy));
        }
        for (url, listed) in &ollama {
            lines.extend(self.ollama_strays(url, listed, asked));
        }
        if unreadable != self.unreadable {
            if let Some(err) = &unreadable {
                lines.push(format!("paddock: {err}"));
            }
            self.unreadable = unreadable;
        }
        if podman != self.podman {
            if let Some(why) = &podman {
                lines.push(format!(
                    "paddock: containers cannot be read ({why}), so each model in a container is measured by its sheep alone"
                ));
            }
            self.podman = podman;
        }
        self.blobs = blobs;
        let tracked = self.tracked(asked, containers.as_ref());
        let bare = self.bare_jobs();
        let listed: Vec<String> = self.blobs.values().map(|(_, blob)| blob.clone()).collect();
        let mut measures = survey::measure(&Inputs {
            tracked: &tracked,
            flock: flock.as_deref().unwrap_or_default(),
            blobs: &listed,
            gpu: gpu.as_ref(),
            cmdlines: &cmdlines,
            bare: &bare,
            parents: &parents,
        });
        measures
            .models
            .retain(|model, _| self.read_after(model, asked));
        // shep cannot describe an empty flock, so an unread flock holding no tracked sheep is empty.
        let flock_unknown = flock.is_none()
            && tracked
                .iter()
                .any(|tracked| matches!(tracked.on, Where::Sheep(_)));
        let ollama_unknown = unanswered.iter().any(|url| {
            self.blobs.keys().any(|(at, _)| at == url)
                || tracked
                    .iter()
                    .any(|tracked| self.on_ollama(&tracked.model, url))
        });
        let sheep_pids: BTreeSet<u32> = tracked
            .iter()
            .filter_map(|tracked| match &tracked.on {
                Where::Sheep(sheep) => Some(survey::pids_of(
                    flock.as_deref().unwrap_or_default(),
                    tracked,
                    sheep,
                )),
                Where::Ollama { .. } => None,
            })
            .flatten()
            .collect();
        let runner_unknown = !unread_cmdlines.is_subset(&sheep_pids);
        let in_container = |model: &ModelName| {
            self.loaded_with
                .get(model)
                .is_some_and(|loaded| loaded.container.is_some())
        };
        // A container podman did not describe may hold GPU memory unseen.
        let container_unknown =
            containers.is_none() && tracked.iter().any(|tracked| in_container(&tracked.model));
        if flock_unknown || ollama_unknown || runner_unknown || container_unknown {
            measures.unaccounted_vram = None;
        }
        let figures = tracked
            .iter()
            .filter_map(|tracked| {
                let measured = measures.models.get(&tracked.model)?;
                // A model in a container podman did not describe is measured in part.
                let known = containers.is_some() || !in_container(&tracked.model);
                let read = match &tracked.on {
                    Where::Sheep(_) => Read {
                        vram: gpu.is_some() && flock.is_some() && known,
                        ram: flock.is_some() && known,
                    },
                    Where::Ollama { blob } => Read {
                        vram: gpu.is_some()
                            && blob.is_some()
                            && !runner_unknown
                            && !unanswered
                                .iter()
                                .any(|url| self.on_ollama(&tracked.model, url)),
                        ram: true,
                    },
                };
                Some((tracked.model.clone(), (tracked.declared, *measured, read)))
            })
            .collect();
        lines.extend(self.drifting.update(&figures));
        let lease_figures: BTreeMap<LeaseId, (Footprint, Measured, Read)> = bare
            .iter()
            .filter_map(|job| {
                let measured = measures.leases.get(&job.lease)?;
                let read = Read {
                    vram: gpu.is_some(),
                    ram: false,
                };
                Some((job.lease, (job.declared, *measured, read)))
            })
            .collect();
        lines.extend(self.drifting_leases.update(&lease_figures));
        self.measures = measures;
        self.measured_at = Some(asked);
        lines
    }

    /// The blob cache for the next survey to start from
    pub fn blobs(&self) -> Blobs {
        self.blobs.clone()
    }

    /// Every bare lease the status lists, as the survey measures it
    pub fn bare_jobs(&self) -> Vec<BareJob> {
        self.book
            .snapshot(self.clock.moment())
            .leases
            .into_iter()
            .filter_map(|view| {
                Some(BareJob {
                    lease: view.id,
                    pid: view.pid,
                    declared: view.footprint?,
                })
            })
            .collect()
    }

    /// The pids bare leases' jobs run under, for a survey to walk GPU processes up to
    pub fn bare_pids(&self) -> BTreeSet<u32> {
        self.bare_jobs().iter().filter_map(BareJob::root).collect()
    }

    /// The book as the status shows it, with the last survey's figures on each model holding memory
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.book.snapshot(self.clock.moment());
        for view in &mut snapshot.models {
            let current = self
                .measured_at
                .is_some_and(|asked| self.read_after(&view.name, asked));
            let measured = self.measures.models.get(&view.name);
            if let (true, true, Some(measured)) = (view.state.holds_now(), current, measured) {
                view.measured = *measured;
                view.drift = self.drifting.contains(&view.name);
            }
        }
        for view in &mut snapshot.leases {
            if view.footprint.is_some()
                && let Some(measured) = self.measures.leases.get(&view.id)
            {
                view.measured = *measured;
                view.drift = self.drifting_leases.contains(&view.id);
            }
        }
        snapshot.unaccounted_vram = self.measures.unaccounted_vram;
        snapshot
    }

    /// Whether `model` was last loaded on the ollama at `url`
    fn on_ollama(&self, model: &ModelName, url: &str) -> bool {
        self.loaded_with.get(model).is_some_and(
            |loaded| matches!(&loaded.backend, Backend::Ollama { url: on, .. } if on == url),
        )
    }

    /// Whether a survey begun at `asked` began after `model`'s last job reported
    fn read_after(&self, model: &ModelName, asked: Instant) -> bool {
        self.settled
            .get(model)
            .is_none_or(|settled| *settled < asked)
    }

    /// Every model holding memory, or holding it when a survey begun at `asked` read the host,
    /// where it runs, and what it counts for
    fn tracked(
        &self,
        asked: Instant,
        containers: Option<&BTreeMap<String, ContainerRead>>,
    ) -> Vec<Tracked> {
        self.book
            .snapshot(self.clock.moment())
            .models
            .into_iter()
            .filter(|view| view.state.holds_now() || !self.read_after(&view.name, asked))
            .filter_map(|view| {
                let loaded = self.loaded_with.get(&view.name)?;
                let on = match &loaded.backend {
                    Backend::Sheep { sheep, .. } => Where::Sheep(sheep.clone()),
                    Backend::Ollama { url, name } => Where::Ollama {
                        blob: self
                            .blobs
                            .get(&(url.clone(), tagged(name)))
                            .map(|(_, blob)| blob.clone()),
                    },
                };
                let container = loaded
                    .container
                    .as_ref()
                    .and_then(|name| containers?.get(name))
                    .cloned();
                Some(Tracked {
                    model: view.name,
                    on,
                    declared: view.footprint,
                    container,
                })
            })
            .collect()
    }
}
