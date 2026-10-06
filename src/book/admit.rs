//! Whether a model fits, what to evict when it does not, and what blocks it.

use std::time::Duration;

use super::{Action, Book, Moment, Priority, Reason, Slot, State};
use crate::{config::ModelName, footprint::Footprint};

/// What keeps a model from being evicted for one waiter
///
/// Ordered as a search for room tries them, so it reaches for a held
/// model only when nothing else will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Guard {
    /// Nothing: it may be evicted.
    Free,
    /// A batch waiter may not evict it until its grace period ends.
    Grace,
    /// It is Reserved or Loading for another waiter.
    Claim,
    /// A lease holds it.
    Held,
}

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
        self.fits(model, &[], |_, slot| slot.state.holds_now())
            && self.fits(model, &[], |name, slot| self.holds_later(name, slot))
    }

    /// Holds or claims memory once the models leaving are gone
    ///
    /// A held model unloading after a crash claims the room its reload needs.
    fn holds_later(&self, model: &ModelName, slot: &Slot) -> bool {
        slot.state.holds_later() || (slot.state == State::Unloading && self.reloads(model))
    }

    /// Whether `model` fits beside the other models `holds` picks, less `freed`
    fn fits(
        &self,
        model: &ModelName,
        freed: &[ModelName],
        holds: impl Fn(&ModelName, &Slot) -> bool,
    ) -> bool {
        let Some(wanted) = self.slots.get(model) else {
            return false;
        };
        let others: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && !freed.contains(name) && holds(name, slot))
            .collect();
        let excluded = others.iter().any(|(name, _)| self.excluded(model, name));
        let figures: Vec<_> = core::iter::once(self.counted(model, wanted))
            .chain(others.iter().map(|(name, slot)| self.counted(name, slot)))
            .collect();
        !excluded && self.config.host.fits(&figures)
    }

    /// Whether `a` and `b` may not be loaded together
    ///
    /// Beyond the config's exclusions, a model the config does not name (a
    /// stand-in, or one removed while it holds memory) excludes every model
    /// on the backend it loaded on, since that backend runs one process.
    pub(super) fn excluded(&self, a: &ModelName, b: &ModelName) -> bool {
        let configured = |name| self.config.models.get(name).map(|model| &model.backend);
        let backend = |name| configured(name).or_else(|| self.slots.get(name)?.loaded_on.as_ref());
        let unconfigured = configured(a).is_none() || configured(b).is_none();
        let shared = unconfigured
            && backend(a)
                .zip(backend(b))
                .is_some_and(|(x, y)| x.same_process(y));
        (a != b && shared) || self.config.excluded(a, b)
    }

    /// What `model` counts for against the host
    ///
    /// The larger of the figures it loaded with and its config's, in each
    /// resource. A model gone from the config counts at what it loaded with.
    pub(super) fn counted(&self, model: &ModelName, slot: &Slot) -> Footprint {
        match self.config.models.get(model) {
            Some(configured) => slot.footprint.larger(configured.footprint),
            None => slot.footprint,
        }
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
        let fits = |freed: &[ModelName]| {
            self.fits(model, freed, |name, slot| self.holds_later(name, slot))
        };
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

    /// Loads `model`, or evicts for it, or names what blocks it
    pub(super) fn make_room(
        &mut self,
        now: Moment,
        model: ModelName,
        priority: Priority,
        out: &mut Vec<Action>,
    ) -> Reason {
        if self.may_load(&model) {
            self.start_load(now, &model, out);
            return Reason::Loading { model };
        }
        let order = self.in_the_way(now, &model, priority);
        let free = order
            .iter()
            .filter(|(guard, _)| *guard == Guard::Free)
            .map(|(_, name)| name.clone())
            .collect();
        if let Some(set) = self.eviction_set(&model, free) {
            self.evict(set, &model, out);
            return Reason::Loading { model };
        }
        self.blocked(now, model, &order)
    }

    /// Why `model` cannot have room, named by the guarded models in the way
    ///
    /// The models are those the search would take if guards were lifted. A
    /// held one is named first, then a claim, then a grace period, so a
    /// refusal names the hardest block.
    fn blocked(&self, now: Moment, model: ModelName, order: &[(Guard, ModelName)]) -> Reason {
        let names = order.iter().map(|(_, name)| name.clone()).collect();
        let Some(set) = self.eviction_set(&model, names) else {
            return Reason::Behind { model };
        };
        let guarded = |wanted: Guard| -> Vec<ModelName> {
            order
                .iter()
                .filter(|(guard, name)| *guard == wanted && set.contains(name))
                .map(|(_, name)| name.clone())
                .collect()
        };
        if let Some(held) = self.held_reason(now, &guarded(Guard::Held)) {
            return held;
        }
        if let Some(claimed) = guarded(Guard::Claim).into_iter().next() {
            return Reason::Behind { model: claimed };
        }
        self.grace_reason(now, &guarded(Guard::Grace))
            .unwrap_or(Reason::Behind { model })
    }

    /// Every model holding or claiming room `model` needs later, in the order
    /// a search for room tries them
    ///
    /// Free and grace models go least recently used first, and a Reserved
    /// claim before a Loading one, since its waiter is the one ahead.
    fn in_the_way(
        &self,
        now: Moment,
        model: &ModelName,
        priority: Priority,
    ) -> Vec<(Guard, ModelName)> {
        let mut found: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && self.holds_later(name, slot))
            .map(|(name, slot)| {
                let in_grace = now < self.used_at(now, name).plus(self.config.grace);
                let guard = match slot.state {
                    State::Reserved | State::Loading => Guard::Claim,
                    _ if self.held(name) => Guard::Held,
                    _ if priority == Priority::Batch && in_grace => Guard::Grace,
                    _ => Guard::Free,
                };
                let age = match slot.state {
                    State::Reserved => Moment(0),
                    State::Loading => Moment(1),
                    _ => slot.last_used,
                };
                (guard, age, name)
            })
            .collect();
        found.sort();
        found
            .into_iter()
            .map(|(guard, _, name)| (guard, name.clone()))
            .collect()
    }

    /// The reason naming the last of `models` to leave its grace period
    fn grace_reason(&self, now: Moment, models: &[ModelName]) -> Option<Reason> {
        models
            .iter()
            .map(|name| (self.used_at(now, name), name))
            .max()
            .map(|(used, name)| Reason::Grace {
                model: name.clone(),
                until: used.plus(self.config.grace),
            })
    }

    /// When `model` was last used, where a request in flight is use now
    pub(super) fn used_at(&self, now: Moment, model: &ModelName) -> Moment {
        match self.slots.get(model) {
            Some(slot) if slot.in_flight > 0 => now,
            Some(slot) => slot.last_used,
            None => Moment(0),
        }
    }

    /// When `model` unloads for sitting idle, if nothing keeps it
    ///
    /// A model gone from the config goes as soon as nothing keeps it. An
    /// unknown one stays until it is evicted.
    pub(super) fn idle_at(&self, model: &ModelName) -> Option<Moment> {
        let slot = self.slots.get(model)?;
        let idle = match self.config.models.get(model) {
            Some(configured) => configured.idle,
            None if slot.unknown => return None,
            None => Duration::ZERO,
        };
        let kept = slot.state != State::Loaded
            || slot.in_flight > 0
            || self.held(model)
            || self.waiters.values().any(|waiter| waiter.model == *model);
        (!kept).then(|| slot.last_used.plus(idle))
    }

    /// Unloads every model idle past its own `idle`
    pub(super) fn unload_idle(&mut self, now: Moment, out: &mut Vec<Action>) {
        let idle: Vec<_> = self
            .slots
            .keys()
            .filter(|model| self.idle_at(model).is_some_and(|at| at <= now))
            .cloned()
            .collect();
        for model in idle {
            if let Some(slot) = self.slots.get_mut(&model) {
                slot.state = State::Unloading;
                slot.for_model = None;
                out.push(Action::Unload(model));
            }
        }
    }

    /// Drops the claim of every Reserved model nothing waits for
    ///
    /// A lease that loads its model again after a crash counts as waiting.
    /// Evictions committed for a dropped claim stand, and no longer name it.
    pub(super) fn drop_unwanted_claims(&mut self) {
        let unwanted: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| {
                slot.state == State::Reserved
                    && !self.reloads(name)
                    && !self.waiters.values().any(|waiter| waiter.model == **name)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for model in unwanted {
            if let Some(slot) = self.slots.get_mut(&model) {
                slot.state = State::Unloaded;
            }
            self.unclaim(&model);
            self.refit(&model);
        }
    }

    /// Stops the evictions committed for `model` from naming it
    pub(super) fn unclaim(&mut self, model: &ModelName) {
        for slot in self.slots.values_mut() {
            if slot.for_model.as_ref() == Some(model) {
                slot.for_model = None;
            }
        }
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
        let backend = self.config.models.get(model).map(|m| m.backend.clone());
        if let Some(slot) = self.slots.get_mut(model) {
            slot.state = State::Loading;
            slot.loaded_on = backend;
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
