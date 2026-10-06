//! Writing `state.json`, and picking up what the last run left when the engine starts.

use std::{collections::VecDeque, time::Duration};

use super::Engine;
use crate::{
    book::Found,
    config::{Backend, ClientName, Model},
    engine::Start,
    saved::{self, Saved, SavedLease, SavedModel},
};

// Request activity is saved at most this stale (Spec readings 9).
const ACTIVITY_SAVE: Duration = Duration::from_secs(60);

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
        for model in &discovered.stand_ins {
            self.seed(model.clone());
        }
        let leases = saved
            .leases
            .into_iter()
            .map(|lease| lease.restored(&self.clock))
            .collect();
        let loaded = discovered
            .loaded
            .into_iter()
            .map(|(model, footprint)| Found {
                model,
                footprint,
                placement: None,
                stray: false,
            })
            .collect();
        let actions = self
            .book
            .restore(self.clock.moment(), loaded, &discovered.stand_ins, leases);
        for (model, error) in discovered.unasked {
            self.book.record_error(self.clock.moment(), model, error);
        }
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

    /// Writes the leases, what each sheep runs and each model holding memory to `state.json`
    ///
    /// Does nothing when the engine has no `state.json`. A failed write is
    /// logged and the engine goes on: the next save may succeed.
    pub(super) fn save(&mut self) {
        let Some(path) = &self.state else {
            return;
        };
        let snapshot = self.book.snapshot(self.clock.moment());
        let saved = Saved {
            leases: snapshot
                .leases
                .into_iter()
                .map(|view| SavedLease::from_view(view, &self.clock))
                .collect(),
            sheep: self
                .on_sheep
                .iter()
                .map(|(sheep, model)| (sheep.clone(), model.clone()))
                .collect(),
            models: snapshot
                .models
                .into_iter()
                .filter(|view| view.state.holds_now())
                .map(|view| {
                    let model = SavedModel {
                        placement: view.placement,
                        stray: view.stray,
                    };
                    (view.name, model)
                })
                .collect(),
            ..Saved::default()
        };
        match saved::store(path, &saved) {
            Ok(()) => self.saved_at = self.clock.moment(),
            Err(err) => eprintln!(
                "paddock: {err}; a restart now would lose what changed since the last save"
            ),
        }
    }

    /// Saves when `client` holds a lease and the last save is [`ACTIVITY_SAVE`] old or more,
    /// so a restart restores its holder's activity at most that stale
    pub(super) fn save_activity(&mut self, client: &ClientName) {
        let holds = self
            .book
            .leases()
            .iter()
            .any(|lease| lease.client == *client);
        if holds && self.clock.moment() >= self.saved_at.plus(ACTIVITY_SAVE) {
            self.save();
        }
    }
}
