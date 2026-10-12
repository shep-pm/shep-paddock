//! A loaded model whose launch settings changed, and its reload once nothing would be cut off.

use super::{Action, Book, Moment, State};
use crate::config::ModelName;

impl Book {
    /// Whether `model` is loaded with launch settings its config has since changed
    ///
    /// Derived from the slot and the config as they are now, so a reload of the config that
    /// brings the settings back clears it. A model the dog did not start has no launch settings
    /// to differ.
    pub(super) fn stale(&self, model: &ModelName) -> bool {
        let Some(slot) = self.slots.get(model) else {
            return false;
        };
        let Some(launched) = &slot.launched else {
            return false;
        };
        // A model gone from the config unloads once nothing names it, whatever it was started with.
        let Some(configured) = self.config.models.get(model) else {
            return false;
        };
        let now = configured.launch_in(slot.placement.as_ref());
        slot.state == State::Loaded && now.as_ref() != Some(launched)
    }

    /// Whether nothing would be cut off by unloading `model`: no lease holds it, no request is in
    /// flight on it, and no waiter names it
    fn unwanted(&self, model: &ModelName) -> bool {
        !self.held(model)
            && self.in_flight_on(model) == 0
            && !self
                .waiters
                .values()
                .any(|waiter| waiter.model.as_ref() == Some(model))
    }

    /// Whether `model` would fit beside everything else now at its config's figures, in some
    /// placement
    fn fits_refreshed(&self, model: &ModelName) -> bool {
        let Some(configured) = self.config.models.get(model) else {
            return false;
        };
        if configured.placements.is_empty() {
            return self.may_load_as(model, configured.footprint);
        }
        configured
            .placements
            .iter()
            .any(|placement| self.may_load_as(model, placement.footprint))
    }

    /// Starts the reload of every stale model that nothing would be cut off from
    ///
    /// It must be unheld, have no request in flight and no waiter, have gone unused for the grace
    /// period, and fit beside everything else at its new figures. One that does not fit stays
    /// stale and is considered again at every event. The reload is an unload, then a load once it
    /// is Unloaded, as an eviction's are, except that the model's reclaimable leases are kept.
    /// Records when the grace periods it waits on end, so a Tick comes then.
    pub(super) fn reload_stale(&mut self, now: Moment, out: &mut Vec<Action>) {
        self.stale_grace.clear();
        let candidates: Vec<_> = self
            .slots
            .keys()
            .filter(|model| self.stale(model) && self.unwanted(model))
            .cloned()
            .collect();
        for model in candidates {
            let grace_ends = self.used_at(now, &model).plus(self.config.grace);
            if now < grace_ends {
                self.stale_grace.push(grace_ends);
            } else if self.fits_refreshed(&model)
                && let Some(slot) = self.slots.get_mut(&model)
            {
                slot.state = State::Unloading;
                slot.for_model = None;
                slot.refreshing = true;
                out.push(Action::Unload(model));
            }
        }
    }
}
