//! Writing `state.json`, and picking up what the last run left when the engine starts.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

use super::Engine;
use crate::{
    book::{Moment, State},
    config::{Backend, ClientName, Model, ModelName},
    engine::Start,
    saved::{self, Saved, SavedLease, SavedModel},
};

// Request activity is saved at most this stale (Spec readings 9).
const ACTIVITY_SAVE: Duration = Duration::from_secs(60);

impl Engine {
    /// Picks up the saved leases and the models discovery found loaded
    ///
    /// Each loaded model is tracked as if the engine had loaded it, in its
    /// restored placement. So its crash is noticed and its unload has a backend.
    /// The book takes each placement and stray flag before the first save,
    /// which would otherwise write over them. An unknown model is tracked under
    /// its stand-in, whose unload stops the sheep or tells ollama to drop it. So
    /// is a model found on a sheep a reload moved it off.
    pub fn restore(&mut self, start: Start) {
        let Start {
            state,
            saved,
            discovered,
            survey: _,
        } = start;
        self.state = state;
        self.saved_models = saved.models;
        self.saved_in_use = saved
            .leases
            .iter()
            .filter(|lease| lease.last_activity.is_none())
            .map(|lease| lease.id)
            .collect();
        let moved = |name: &ModelName| discovered.stand_ins.iter().any(|on| on.name == *name);
        for found in &discovered.loaded {
            if moved(&found.model) {
                continue;
            }
            if let Some(model) = self.config.models.get(&found.model) {
                let model = match &found.placement {
                    Some(placement) => model.placed(placement),
                    None => model.clone(),
                };
                self.seed(model);
            }
        }
        for model in &discovered.stand_ins {
            self.seed(model.clone());
        }
        let leases = saved
            .leases
            .into_iter()
            .filter_map(|lease| {
                let id = lease.id;
                let restored = lease.restored(&self.clock);
                if restored.is_none() {
                    eprintln!(
                        "paddock: dropping saved lease {id} that names no model and no footprint"
                    );
                }
                restored
            })
            .collect();
        let actions = self.book.restore(
            self.clock.moment(),
            discovered.loaded,
            &discovered.stand_ins,
            leases,
        );
        for (model, error) in discovered.unasked {
            self.book.record_error(self.clock.moment(), model, error);
        }
        let mut queue = VecDeque::new();
        self.apply(actions, &mut queue);
        while let Some(event) = queue.pop_front() {
            self.feed(event);
        }
        self.save_changes();
    }

    /// Tracks `model` as loaded on its backend, and on no other sheep
    pub(super) fn seed(&mut self, model: Model) {
        self.on_sheep
            .retain(|sheep, named| *named != model.name || model.backend.sheep() == Some(sheep));
        if let Backend::Sheep { sheep, .. } = &model.backend {
            self.on_sheep.insert(sheep.clone(), model.name.clone());
        }
        *self.loads.entry(model.name.clone()).or_default() += 1;
        self.loaded_with.insert(model.name.clone(), model);
    }

    /// Drops `sheep`'s record if it names `model`, which the dog stopped there, and says if it did
    pub(super) fn stopped_on_sheep(&mut self, model: &ModelName, sheep: &str) -> bool {
        let recorded = self.on_sheep.get(sheep) == Some(model);
        if recorded {
            self.on_sheep.remove(sheep);
        }
        recorded
    }

    /// Drops the record of each sheep the config names no model on, unless its model still runs there
    ///
    /// Such a sheep's model stays recorded until the dog stops it. Saves at
    /// once when a record goes, since a restart would read it.
    pub(super) fn forget_unnamed_sheep(&mut self) {
        let before = self.on_sheep.len();
        let (config, book, loaded_with) = (&self.config, &self.book, &self.loaded_with);
        self.on_sheep.retain(|sheep, model| {
            let named = config
                .models
                .values()
                .any(|configured| configured.backend.sheep() == Some(sheep));
            let runs_there = book
                .state(model)
                .is_some_and(|state| state != State::Unloaded)
                && loaded_with
                    .get(model)
                    .is_some_and(|loaded| loaded.backend.sheep() == Some(sheep));
            named || runs_there
        });
        if self.on_sheep.len() != before {
            self.save();
        }
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
        let leases = self.book.leases();
        let in_use: BTreeSet<_> = leases
            .iter()
            .filter(|lease| lease.in_use)
            .map(|lease| lease.id)
            .collect();
        let saved = Saved {
            leases: leases
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
        if stored.is_ok() {
            self.saved_in_use = in_use;
        }
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

    /// Saves at once when a lease saved as in use left use, or the models holding memory changed
    ///
    /// Otherwise it saves what is due. A change of placement or stray flag
    /// counts as a change of the models. A lease the file shows out of use
    /// needs no save when its use ends: the arrival that began that use
    /// marked activity the deferred save will carry, end time included.
    pub(super) fn save_changes(&mut self) {
        if self.state.is_none() {
            return;
        }
        let use_ended = !self.saved_in_use.is_empty() && {
            let still = self.book.in_use_leases();
            self.saved_in_use
                .iter()
                .any(|id| !still.contains(id) && self.book.lease(*id).is_some())
        };
        // Both are in name order, so they compare item by item without building a map.
        let saved = self
            .saved_models
            .iter()
            .map(|(name, kept)| (name, kept.placement.as_ref(), kept.stray));
        if use_ended || !self.book.holding().eq(saved) {
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
