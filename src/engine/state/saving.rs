//! Writing `state.json`, and picking up what the last run left when the engine starts.

use std::collections::VecDeque;

use super::Engine;
use crate::{
    config::{Backend, Model},
    engine::Start,
    saved::{self, Saved, SavedLease},
};

impl Engine {
    /// Picks up the saved leases and the models discovery found loaded
    ///
    /// Each loaded model is tracked as if the engine had loaded it, so its
    /// crash is noticed and its unload has a backend to use. An unknown
    /// model is tracked under its stand-in, whose unload stops the sheep or
    /// tells ollama to drop it.
    pub fn restore(&mut self, start: Start) {
        let Start {
            state,
            saved,
            discovered,
        } = start;
        self.state = state;
        for (name, _) in &discovered.loaded {
            if let Some(model) = self.config.models.get(name).cloned() {
                self.seed(model);
            }
        }
        for model in discovered.stand_ins {
            self.seed(model);
        }
        let leases = saved
            .leases
            .into_iter()
            .map(|lease| lease.restored(&self.clock))
            .collect();
        let actions = self
            .book
            .restore(self.clock.moment(), discovered.loaded, leases);
        let mut queue = VecDeque::new();
        self.apply(actions, &mut queue);
        while let Some(event) = queue.pop_front() {
            self.feed(event);
        }
    }

    /// Tracks `model` as loaded on its backend
    pub(super) fn seed(&mut self, model: Model) {
        if let Backend::Sheep { sheep, .. } = &model.backend {
            self.on_sheep.insert(sheep.clone(), model.name.clone());
        }
        *self.loads.entry(model.name.clone()).or_default() += 1;
        self.loaded_with.insert(model.name.clone(), model);
    }

    /// Writes the leases and what each sheep runs to `state.json`, if the engine has one
    ///
    /// A failed write is logged and the engine goes on: the next save may succeed.
    pub(super) fn save(&self) {
        let Some(path) = &self.state else {
            return;
        };
        let saved = Saved {
            leases: self
                .book
                .leases()
                .into_iter()
                .map(|view| SavedLease::from_view(view, &self.clock))
                .collect(),
            sheep: self
                .on_sheep
                .iter()
                .map(|(sheep, model)| (sheep.clone(), model.clone()))
                .collect(),
            ..Saved::default()
        };
        if let Err(err) = saved::store(path, &saved) {
            eprintln!("paddock: {err}; a restart now would lose what changed since the last save");
        }
    }
}
