//! Whether a model fits, and what to evict when it does not.

use super::{Action, Book, Moment, State};
use crate::config::ModelName;

impl State {
    /// Holds memory now
    fn holds_now(self) -> bool {
        matches!(
            self,
            Self::Loading | Self::Loaded | Self::Evicting | Self::Unloading
        )
    }

    /// Holds or claims memory once the models leaving are gone
    fn holds_later(self) -> bool {
        matches!(self, Self::Reserved | Self::Loading | Self::Loaded)
    }
}

impl Book {
    /// Whether `model` may start loading
    ///
    /// It must fit beside the memory held now, and beside the memory held or
    /// claimed once the models leaving are gone, with no exclusion in either.
    pub(super) fn may_load(&self, model: &ModelName) -> bool {
        self.fits(model, &[], State::holds_now) && self.fits(model, &[], State::holds_later)
    }

    /// Whether `model` fits beside the other models `holds` picks, less `freed`
    fn fits(&self, model: &ModelName, freed: &[ModelName], holds: fn(State) -> bool) -> bool {
        let Some(wanted) = self.slots.get(model) else {
            return false;
        };
        let others: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && !freed.contains(name) && holds(slot.state))
            .collect();
        let excluded = others
            .iter()
            .any(|(name, _)| self.config.excluded(model, name));
        let held = others.iter().map(|(_, slot)| &slot.footprint);
        !excluded
            && self
                .config
                .host
                .fits(core::iter::once(&wanted.footprint).chain(held))
    }

    /// The fewest least recently used candidates whose eviction lets `model` fit
    ///
    /// Only the memory held once the models leaving are gone counts, so
    /// nothing is evicted for room an unload under way will free. Candidates
    /// are added oldest first until `model` fits, then any not needed are put
    /// back, newest first. The set is empty when that room is already coming.
    pub(super) fn eviction_set(
        &self,
        model: &ModelName,
        candidates: Vec<ModelName>,
    ) -> Option<Vec<ModelName>> {
        let fits = |freed: &[ModelName]| self.fits(model, freed, State::holds_later);
        let mut chosen = Vec::new();
        let mut candidates = candidates.into_iter();
        while !fits(&chosen) {
            chosen.push(candidates.next()?);
        }
        for at in (0..chosen.len()).rev() {
            let kept = chosen.remove(at);
            if !fits(&chosen) {
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
            State::Unloaded | State::Loaded | State::Evicting | State::Unloading => None,
        };
        self.slots
            .iter()
            .filter(|(name, _)| *name != model)
            .filter_map(|(name, slot)| rank(slot.state).map(|rank| (rank, name)))
            .min()
            .map_or_else(|| model.clone(), |(_, name)| name.clone())
    }

    /// Starts loading every Reserved model the room now allows, by name
    pub(super) fn load_reserved(&mut self, now: Moment, out: &mut Vec<Action>) {
        let reserved: Vec<_> = self
            .slots
            .iter()
            .filter(|(_, slot)| slot.state == State::Reserved)
            .map(|(name, _)| name.clone())
            .collect();
        for model in reserved {
            if self.may_load(&model) {
                self.start_load(now, &model, out);
            }
        }
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
