//! Writing `state.json`, and picking up what the last run left when the engine starts.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

use super::Engine;
use crate::{
    book::{Found, LeaseId, Moment},
    config::{Backend, ClientName, Model, ModelName},
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
        self.saved_models = saved.models;
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
        self.save_changes(&BTreeSet::new());
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
    /// logged and tried again [`ACTIVITY_SAVE`] later, unless a save comes first.
    pub(super) fn save(&mut self) {
        let Some(path) = &self.state else {
            return;
        };
        let models = self.holding();
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
            models: models.clone(),
            ..Saved::default()
        };
        let stored = saved::store(path, &saved);
        self.saved_models = models;
        self.saved_at = self.clock.moment();
        self.unsaved = stored.is_err();
        if let Err(err) = stored {
            eprintln!("paddock: {err}; a restart now would lose what changed since the last save");
        }
    }

    /// Marks `client`'s request for `model` as activity to save, when `client` holds a lease on it
    ///
    /// The feed that applies the request saves it once the last save is
    /// [`ACTIVITY_SAVE`] old, and the run loop's `Tick` saves it then otherwise.
    pub(super) fn mark_activity(&mut self, client: &ClientName, model: &ModelName) {
        if self.state.is_some() && self.book.holds(client, model) {
            self.unsaved = true;
        }
    }

    /// Saves at once when a lease of `in_use` left use or the models holding memory changed
    ///
    /// Otherwise it saves what is due. A change of placement or stray flag
    /// counts as a change of the models.
    pub(super) fn save_changes(&mut self, in_use: &BTreeSet<LeaseId>) {
        if self.state.is_none() {
            return;
        }
        let still = self.book.in_use_leases();
        let use_ended = in_use
            .iter()
            .any(|id| !still.contains(id) && self.book.lease(*id).is_some());
        if use_ended || self.holding() != self.saved_models {
            self.save();
        } else {
            self.save_due();
        }
    }

    /// Each model holding memory, as `state.json` names it
    fn holding(&self) -> BTreeMap<ModelName, SavedModel> {
        self.book
            .holding()
            .map(|(name, placement, stray)| {
                let model = SavedModel {
                    placement: placement.cloned(),
                    stray,
                };
                (name.clone(), model)
            })
            .collect()
    }

    /// Saves what `state.json` lacks, once the last save is [`ACTIVITY_SAVE`] old
    pub(super) fn save_due(&mut self) {
        if self
            .save_deadline()
            .is_some_and(|due| self.clock.moment() >= due)
        {
            self.save();
        }
    }

    /// When [`save_due`](Self::save_due) next saves, if anything waits to be saved
    pub(super) fn save_deadline(&self) -> Option<Moment> {
        self.unsaved.then(|| self.saved_at.plus(ACTIVITY_SAVE))
    }
}
