//! What the survey measured: each model's figures and drift, and the GPU memory nobody holds.

use tokio::time::Instant;

use super::Engine;
use crate::{
    book::Snapshot,
    config::{Backend, ModelName, tagged},
    engine::survey::{Blobs, Reading},
    survey::{self, Inputs, Tracked, Where},
};

impl Engine {
    /// Measures each model holding memory from one survey's reading, and logs drift's start and end
    ///
    /// A model whose job reported after the survey began is not measured: the reading may be
    /// from before its load or unload. What its tree held is still its own, not unaccounted.
    pub fn surveyed(&mut self, reading: Reading) {
        let Reading {
            asked,
            flock,
            blobs,
            gpu,
            unreadable,
            cmdlines,
        } = reading;
        if unreadable != self.unreadable {
            if let Some(err) = &unreadable {
                eprintln!("paddock: {err}");
            }
            self.unreadable = unreadable;
        }
        self.blobs = blobs;
        let tracked = self.tracked(asked);
        let listed: Vec<String> = self.blobs.values().map(|(_, blob)| blob.clone()).collect();
        let mut measures = survey::measure(&Inputs {
            tracked: &tracked,
            flock: flock.as_deref().unwrap_or_default(),
            blobs: &listed,
            gpu: gpu.as_ref(),
            cmdlines: &cmdlines,
        });
        measures
            .models
            .retain(|model, _| self.read_after(model, asked));
        let figures = tracked
            .iter()
            .filter_map(|tracked| {
                let measured = measures.models.get(&tracked.model)?;
                Some((tracked.model.clone(), (tracked.declared, *measured)))
            })
            .collect();
        for line in self.drifting.update(&figures) {
            eprintln!("{line}");
        }
        self.measures = measures;
        self.measured_at = Some(asked);
    }

    /// The blob cache for the next survey to start from
    pub fn blobs(&self) -> Blobs {
        self.blobs.clone()
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
        snapshot.unaccounted_vram = self.measures.unaccounted_vram;
        snapshot
    }

    /// Whether a survey begun at `asked` began after `model`'s last job reported
    fn read_after(&self, model: &ModelName, asked: Instant) -> bool {
        self.settled
            .get(model)
            .is_none_or(|settled| *settled < asked)
    }

    /// Every model holding memory, or holding it when a survey begun at `asked` read the host,
    /// where it runs, and what it counts for
    fn tracked(&self, asked: Instant) -> Vec<Tracked> {
        self.book
            .snapshot(self.clock.moment())
            .models
            .into_iter()
            .filter(|view| view.state.holds_now() || !self.read_after(&view.name, asked))
            .filter_map(|view| {
                let on = match &self.loaded_with.get(&view.name)?.backend {
                    Backend::Sheep { sheep, .. } => Where::Sheep(sheep.clone()),
                    Backend::Ollama { url, name } => Where::Ollama {
                        blob: self
                            .blobs
                            .get(&(url.clone(), tagged(name)))
                            .map(|(_, blob)| blob.clone()),
                    },
                };
                Some(Tracked {
                    model: view.name,
                    on,
                    declared: view.footprint,
                })
            })
            .collect()
    }
}
