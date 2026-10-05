//! Whether a model fits, and what to evict when it does not.

use super::{Action, Book, Moment, State};
use crate::config::ModelName;

impl State {
    /// Holds memory now, or is about to
    fn holds_now(self) -> bool {
        matches!(
            self,
            Self::Loading | Self::Loaded | Self::Evicting | Self::Unloading
        )
    }

    /// Holds memory once the unloads under way finish
    fn holds_then(self) -> bool {
        matches!(self, Self::Reserved | Self::Loading | Self::Loaded)
    }
}

impl Book {
    /// Whether `model` fits if the models in `freed` were gone
    ///
    /// A Reserved model and the models evicted for it claim the same room,
    /// so each side is summed apart: the memory held now, and the memory
    /// held once the unloads under way finish.
    pub(super) fn fits(&self, model: &ModelName, freed: &[ModelName]) -> bool {
        let Some(wanted) = self.slots.get(model) else {
            return false;
        };
        let others = || {
            self.slots
                .iter()
                .filter(|(name, _)| *name != model && !freed.contains(name))
        };
        let excluded = others()
            .any(|(name, slot)| slot.state != State::Unloaded && self.config.excluded(model, name));
        let side = |holds: fn(State) -> bool| {
            let held = others()
                .filter(|(_, slot)| holds(slot.state))
                .map(|(_, slot)| &slot.footprint);
            self.config
                .host
                .fits(core::iter::once(&wanted.footprint).chain(held))
        };
        !excluded && side(State::holds_now) && side(State::holds_then)
    }

    /// The fewest least recently used candidates whose eviction lets `model` fit
    ///
    /// Candidates are added oldest first until `model` fits, then any whose
    /// room turned out not to be needed are put back, newest first.
    pub(super) fn eviction_set(
        &self,
        model: &ModelName,
        candidates: Vec<ModelName>,
    ) -> Option<Vec<ModelName>> {
        let mut chosen = Vec::new();
        let mut candidates = candidates.into_iter();
        while !self.fits(model, &chosen) {
            chosen.push(candidates.next()?);
        }
        for at in (0..chosen.len()).rev() {
            let kept = chosen.remove(at);
            if !self.fits(model, &chosen) {
                chosen.insert(at, kept);
            }
        }
        Some(chosen)
    }

    /// The Loaded models `model` may evict, least recently used first
    pub(super) fn candidates(&self, model: &ModelName) -> Vec<ModelName> {
        let mut loaded: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && slot.state == State::Loaded)
            .collect();
        loaded.sort_by_key(|(name, slot)| (slot.last_used, *name));
        loaded.into_iter().map(|(name, _)| name.clone()).collect()
    }

    /// The model a waiter on `model` is behind when no eviction makes room
    ///
    /// A Reserved model comes first, since its waiter is the one ahead.
    pub(super) fn blocker(&self, model: &ModelName) -> ModelName {
        let rank = |state| match state {
            State::Reserved => Some(0),
            State::Loading => Some(1),
            State::Evicting | State::Unloading => Some(2),
            State::Unloaded | State::Loaded => None,
        };
        self.slots
            .iter()
            .filter(|(name, _)| *name != model)
            .filter_map(|(name, slot)| rank(slot.state).map(|rank| (rank, name)))
            .min()
            .map_or_else(|| model.clone(), |(_, name)| name.clone())
    }

    pub(super) fn start_load(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        if let Some(slot) = self.slots.get_mut(model) {
            slot.state = State::Loading;
            slot.load_started = now;
            slot.failed_once = false;
            out.push(Action::Load(model.clone()));
        }
    }

    /// Commits an eviction: `set` leaves, and its room is claimed for `model`
    pub(super) fn evict(&mut self, set: Vec<ModelName>, model: &ModelName, out: &mut Vec<Action>) {
        for name in set {
            let Some(slot) = self.slots.get_mut(&name) else {
                continue;
            };
            slot.for_model = Some(model.clone());
            if slot.in_flight == 0 {
                slot.state = State::Unloading;
                out.push(Action::Unload(name));
            } else {
                slot.state = State::Evicting;
            }
        }
        if let Some(slot) = self.slots.get_mut(model) {
            slot.state = State::Reserved;
        }
    }
}
