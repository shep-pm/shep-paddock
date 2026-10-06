//! Models something other than the dog loaded.

use super::Engine;
use crate::{
    book::{Event, State},
    config::ModelName,
    discover,
};

impl Engine {
    /// Whether nothing the dog loads, runs or stops is on `sheep`
    ///
    /// A model on `sheep` is the dog's while the book has it in any state
    /// but Unloaded, whether the config puts it there or the dog last
    /// seeded it there.
    pub(super) fn untracked(&self, sheep: &str) -> bool {
        let holding = |model: &ModelName| {
            self.book
                .state(model)
                .is_some_and(|state| state != State::Unloaded)
        };
        let configured = self
            .config
            .models
            .values()
            .filter(|model| model.backend.sheep() == Some(sheep))
            .any(|model| holding(&model.name));
        let seeded = self.on_sheep.get(sheep).is_some_and(holding);
        !configured
            && !seeded
            && !self.stopping.contains(sheep)
            && !self.stop_skipped.contains_key(sheep)
    }

    /// Counts what runs on `sheep`, which the dog did not start, as a stray
    ///
    /// It counts as [`discover::unrecorded`] says, and is seeded so its
    /// eviction stops the sheep. A sheep no model names is not counted.
    pub(super) fn stray_sheep(&mut self, sheep: &str) {
        let Some(model) = discover::unrecorded(&self.config, sheep) else {
            return;
        };
        eprintln!(
            "paddock: sheep {sheep} came online without the dog; counting it as {}",
            model.name
        );
        self.seed(model.clone());
        self.feed(Event::StrayFound {
            model: model.name,
            footprint: model.footprint,
            backend: model.backend,
        });
    }
}
