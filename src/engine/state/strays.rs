//! Models something other than the dog loaded: from `online` events, the flock and `/api/ps`.

use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};
use tokio::time::Instant;

use super::Engine;
use crate::{
    backend::OllamaLoaded,
    book::{Event, State},
    config::{Backend, Model, ModelName, tagged},
    discover,
};

impl Engine {
    /// Whether nothing the dog loads, runs or stops is on `sheep`
    ///
    /// A model on `sheep` is the dog's while the book has it in any state
    /// but Unloaded, whether the config puts it there or the dog last
    /// seeded it there. The config's view covers what the book holds but
    /// never seeded on `sheep`: a model Reserved there, or one a reload
    /// moved there while it still runs on its old sheep.
    pub(super) fn untracked(&self, sheep: &str) -> bool {
        let holding = |model: &ModelName| {
            self.book
                .state(model)
                .is_some_and(|state| state != State::Unloaded)
        };
        !self.models_on(sheep).any(holding)
            && !self.stopping.contains(sheep)
            // A skipped stop always has a load on its sheep, which `process` sees as busy
            // too; this keeps `untracked` whole for a caller that knows no jobs.
            && !self.stop_skipped.contains_key(sheep)
    }

    /// Every model on `sheep`: configured there, or last seeded there
    fn models_on<'a>(&'a self, sheep: &'a str) -> impl Iterator<Item = &'a ModelName> {
        self.config
            .models
            .values()
            .filter(move |model| model.backend.sheep() == Some(sheep))
            .map(|model| &model.name)
            .chain(self.on_sheep.get(sheep))
    }

    /// Counts what runs on `sheep`, which the dog did not start, as a stray, and names it
    ///
    /// It counts as [`discover::unrecorded`] says, and is seeded so its
    /// eviction stops the sheep. A sheep no model names is not counted, nor
    /// is one the book would not take, whose seed would hide what runs there.
    pub(super) fn stray_sheep(&mut self, sheep: &str) -> Option<ModelName> {
        let model = discover::unrecorded(&self.config, sheep)?;
        if !self.book.takes_stray(&model.name, &model.backend) {
            return None;
        }
        let name = model.name.clone();
        self.count_stray(model);
        Some(name)
    }

    /// Seeds `model` so its eviction stops what runs, and has the book count it as a stray
    fn count_stray(&mut self, model: Model) {
        self.seed(model.clone());
        self.feed(Event::StrayFound {
            model: model.name,
            footprint: model.footprint,
            backend: model.backend,
        });
    }

    /// Forgets each stray in `gone`, returning a line to log for each
    fn forget(&mut self, gone: Vec<ModelName>) -> Vec<String> {
        let mut lines = Vec::new();
        for model in gone {
            lines.push(format!(
                "paddock: {model}, a stray, no longer runs; forgetting it"
            ));
            self.feed(Event::BackendExited { model });
        }
        lines
    }

    /// Notes that every model on `sheep` may have changed state now
    pub(super) fn touch_sheep(&mut self, sheep: &str) {
        let now = Instant::now();
        let on: Vec<ModelName> = self.models_on(sheep).cloned().collect();
        for model in on {
            self.touched.insert(model, now);
        }
    }

    /// Whether a model on `sheep` changed state at or after `asked`
    fn sheep_touched_since(&self, sheep: &str, asked: Instant) -> bool {
        self.models_on(sheep)
            .any(|model| self.touched_since(model, asked))
    }

    fn touched_since(&self, model: &ModelName, asked: Instant) -> bool {
        self.touched.get(model).is_some_and(|at| *at >= asked)
    }

    /// The strays in the book that hold memory, with what each was found on
    fn strays(&self) -> Vec<(ModelName, Backend)> {
        self.book
            .snapshot(self.clock.moment())
            .models
            .into_iter()
            .filter(|view| view.stray && view.state != State::Unloaded)
            .filter_map(|view| {
                let backend = self.loaded_with.get(&view.name)?.backend.clone();
                Some((view.name, backend))
            })
            .collect()
    }

    /// Counts the Online sheep the flock shows that nothing tracks, and forgets the sheep strays
    /// it shows not running, returning a line to log for each
    ///
    /// A sheep a job runs on, as `busy` tells, is the dog's. Without a flock nothing changes.
    pub(super) fn sheep_strays(
        &mut self,
        flock: Option<&[ProcessInfo]>,
        asked: Instant,
        busy: &impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let Some(flock) = flock else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        for row in flock.iter().filter(|row| row.status == ProcStatus::Online) {
            let sheep = row.name.as_str();
            if busy(sheep) || !self.untracked(sheep) || self.sheep_touched_since(sheep, asked) {
                continue;
            }
            if let Some(model) = self.stray_sheep(sheep) {
                lines.push(format!(
                    "paddock: sheep {sheep} is running without the dog; counting it as {model}"
                ));
            }
        }
        let running = |sheep: &str| {
            flock.iter().any(|row| {
                row.name == sheep && matches!(row.status, ProcStatus::Starting | ProcStatus::Online)
            })
        };
        let gone: Vec<ModelName> = self
            .strays()
            .into_iter()
            .filter(|(_, backend)| {
                backend.sheep().is_some_and(|sheep| {
                    !running(sheep) && !busy(sheep) && !self.sheep_touched_since(sheep, asked)
                })
            })
            .map(|(model, _)| model)
            .collect();
        lines.extend(self.forget(gone));
        lines
    }

    /// Counts each model the ollama at `url` lists that the dog does not hold, and forgets the
    /// strays on it the listing no longer names, returning a line to log for each
    pub(super) fn ollama_strays(
        &mut self,
        url: &str,
        listed: &[OllamaLoaded],
        asked: Instant,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut taken: Vec<ModelName> = self
            .book
            .snapshot(self.clock.moment())
            .models
            .into_iter()
            .map(|view| view.name)
            .collect();
        for loaded in listed {
            let on = Backend::Ollama {
                url: url.to_owned(),
                name: loaded.name.clone(),
            };
            let held = self.loaded_with.values().any(|model| {
                model.backend.same_process(&on)
                    && self
                        .book
                        .state(&model.name)
                        .is_some_and(|state| state != State::Unloaded)
            });
            if held {
                continue;
            }
            let Some(model) = discover::ollama_stray(&self.config, &taken, url, loaded.clone())
            else {
                continue;
            };
            if !self.book.takes_stray(&model.name, &model.backend)
                || self.touched_since(&model.name, asked)
            {
                continue;
            }
            lines.push(format!(
                "paddock: ollama {} has {} loaded without the dog; counting it as {}",
                discover::ollama_backend(&self.config, url),
                loaded.name,
                model.name
            ));
            taken.push(model.name.clone());
            self.count_stray(model);
        }
        let names: Vec<String> = listed.iter().map(|loaded| tagged(&loaded.name)).collect();
        let gone: Vec<ModelName> = self
            .strays()
            .into_iter()
            .filter(|(model, backend)| match backend {
                Backend::Ollama { url: at, name } => {
                    at == url && !names.contains(&tagged(name)) && !self.touched_since(model, asked)
                }
                Backend::Sheep { .. } => false,
            })
            .map(|(model, _)| model)
            .collect();
        lines.extend(self.forget(gone));
        lines
    }
}
