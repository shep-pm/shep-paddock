//! What backends report: requests ending, loads finishing or failing, unloads and exits.

use super::{Action, Book, LoadError, Moment, State};
use crate::config::{ClientName, ModelName};

// The spec's figure for how many load failures the status keeps.
const ERRORS_KEPT: usize = 20;

impl Book {
    /// Counts a request's end as use of its model, and unloads an evicted model it drains
    pub(super) fn finish(
        &mut self,
        now: Moment,
        client: &ClientName,
        model: &ModelName,
        out: &mut Vec<Action>,
    ) {
        self.end_use(now, client, model);
        let drained = self.in_flight_on(model) == 0;
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        slot.last_used = now;
        if slot.state == State::Evicting && drained {
            slot.state = State::Unloading;
            out.push(Action::Unload(model.clone()));
        }
    }

    pub(super) fn loaded(&mut self, now: Moment, model: &ModelName) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        if slot.state == State::Loading {
            slot.state = State::Loaded;
            slot.load_took = Some(now.since(slot.load_started));
            // Grace from the load, so a batch waiter cannot evict it the moment it lands.
            slot.last_used = now;
            self.reload_on_crash(model, true);
        }
    }

    pub(super) fn load_failed(
        &mut self,
        now: Moment,
        model: &ModelName,
        error: String,
        out: &mut Vec<Action>,
    ) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        if slot.state != State::Loading {
            return;
        }
        // A model gone from the config cannot be loaded again, so its first failure is final.
        if !slot.failed_once && self.config.models.contains_key(model) {
            slot.failed_once = true;
            slot.load_started = now;
            out.push(Action::Load(model.clone()));
            return;
        }
        slot.state = State::Unloaded;
        slot.failed_once = false;
        self.refit(model);
        self.reload_on_crash(model, false);
        self.fail_waiters(
            now,
            |waiter| (waiter.model == *model).then(|| error.clone()),
            out,
        );
        self.record_error(now, model.clone(), error);
    }

    /// Adds `error` against `model` to the status's errors, which keep the last [`ERRORS_KEPT`]
    pub fn record_error(&mut self, now: Moment, model: ModelName, error: String) {
        self.errors.push_back(LoadError {
            model,
            at: now,
            error,
        });
        if self.errors.len() > ERRORS_KEPT {
            self.errors.pop_front();
        }
    }

    /// Frees a model the book was unloading or evicting, and ignores any other
    pub(super) fn unloaded(&mut self, model: &ModelName) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        if !matches!(slot.state, State::Unloading | State::Evicting) {
            return;
        }
        slot.state = State::Unloaded;
        slot.for_model = None;
        self.refit(model);
    }

    /// Unloads a model whose backend exited, ending its reclaimable leases
    ///
    /// Its held leases load it again; a reclaimable lease's holder takes a new one.
    pub(super) fn exited(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(state) = self.state(model) else {
            return;
        };
        match state {
            State::Loaded | State::Evicting => {
                self.reclaim(model, out);
                if let Some(slot) = self.slots.get_mut(model) {
                    slot.state = State::Unloading;
                }
                out.push(Action::Unload(model.clone()));
            }
            State::Loading => {
                let error = "backend exited while loading".to_owned();
                self.load_failed(now, model, error, out);
            }
            State::Unloaded | State::Reserved | State::Unloading => {}
        }
    }

    /// Resets an Unloaded model to its config's figures and no placement
    ///
    /// An Unloaded model the config no longer names is forgotten. A model
    /// holding memory keeps the figures and placement it loaded with until it unloads.
    pub(super) fn refit(&mut self, model: &ModelName) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        match self.config.models.get(model) {
            Some(configured) => {
                slot.unknown = false;
                if slot.state == State::Unloaded {
                    slot.footprint = configured.footprint;
                    slot.placement = None;
                }
            }
            None if slot.state == State::Unloaded => {
                self.slots.remove(model);
            }
            None => {}
        }
    }
}
