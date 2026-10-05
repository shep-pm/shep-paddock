//! What backends report: requests ending, loads finishing or failing, unloads and exits.

use super::{Action, Book, LoadError, Moment, State};
use crate::config::ModelName;

// The spec's figure for how many load failures the status keeps.
const ERRORS_KEPT: usize = 20;

impl Book {
    pub(super) fn finish(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        slot.in_flight = slot.in_flight.saturating_sub(1);
        slot.last_used = now;
        if slot.state == State::Evicting && slot.in_flight == 0 {
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
        if !slot.failed_once {
            slot.failed_once = true;
            slot.load_started = now;
            out.push(Action::Load(model.clone()));
            return;
        }
        slot.state = State::Unloaded;
        slot.failed_once = false;
        self.refit(model);
        self.reload_on_crash(model, false);
        self.waiters.retain(|_, waiter| {
            if waiter.model != *model {
                return true;
            }
            out.push(Action::Fail {
                waiter: waiter.id,
                error: error.clone(),
            });
            false
        });
        self.errors.push_back(LoadError {
            model: model.clone(),
            at: now,
            error,
        });
        if self.errors.len() > ERRORS_KEPT {
            self.errors.pop_front();
        }
    }

    pub(super) fn unloaded(&mut self, model: &ModelName) {
        if let Some(slot) = self.slots.get_mut(model) {
            slot.state = State::Unloaded;
            slot.for_model = None;
        }
        self.refit(model);
    }

    pub(super) fn exited(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        match slot.state {
            State::Loaded | State::Evicting => {
                slot.state = State::Unloading;
                out.push(Action::Unload(model.clone()));
            }
            State::Loading => {
                let error = "backend exited while loading".to_owned();
                self.load_failed(now, model, error, out);
            }
            State::Unloaded | State::Reserved | State::Unloading => {}
        }
    }

    /// Gives an Unloaded model its config's figures, or forgets it if it has none
    ///
    /// A model holding memory keeps the footprint it loaded with until it unloads.
    pub(super) fn refit(&mut self, model: &ModelName) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        match self.config.models.get(model) {
            Some(configured) => {
                slot.unknown = false;
                if slot.state == State::Unloaded {
                    slot.footprint = configured.footprint;
                }
            }
            None if slot.state == State::Unloaded => {
                self.slots.remove(model);
            }
            None => {}
        }
    }
}
