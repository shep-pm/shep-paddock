//! Whether a model fits, what to evict when it does not, and what blocks it.

use std::time::Duration;

use super::{Action, Book, Moment, Priority, Reason, Slot, State, Taker};
use crate::{
    config::{ModelName, PlacementName},
    footprint::Footprint,
};

/// What keeps a model from being evicted for one waiter
///
/// Ordered as a search for room tries them, so it reaches for a held
/// model only when nothing else will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Guard {
    /// Nothing: it may be evicted.
    Free,
    /// A batch waiter may not evict it until its grace period ends.
    Grace,
    /// It is Reserved or Loading for another waiter.
    Claim,
    /// A lease holds it.
    Held,
}

/// Which memory a fit counts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Span {
    /// What is held now.
    Now,
    /// What is held or claimed once the models leaving are gone.
    Later,
}

impl State {
    /// Holds memory now
    pub(crate) fn holds_now(self) -> bool {
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
    /// Whether `model` may start loading at the figures it counts at now
    pub(super) fn may_load(&self, model: &ModelName) -> bool {
        self.slots
            .get(model)
            .is_some_and(|slot| self.may_load_as(model, self.counted(model, slot)))
    }

    /// Whether `model` may start loading at `wanted`
    ///
    /// It must fit beside the memory held now, and beside the memory held or
    /// claimed once the models leaving are gone, with no exclusion in either.
    fn may_load_as(&self, model: &ModelName, wanted: Footprint) -> bool {
        self.fits(Some(model), wanted, &[], Span::Now, None)
            && self.fits(Some(model), wanted, &[], Span::Later, None)
    }

    /// Holds or claims memory once the models leaving are gone
    ///
    /// A held model unloading after a crash claims the room its reload needs.
    fn holds_later(&self, model: &ModelName, slot: &Slot) -> bool {
        slot.state.holds_later() || (slot.state == State::Unloading && self.reloads(model))
    }

    /// Whether `model`'s slot holds memory in `span`
    fn holds_in(&self, model: &ModelName, slot: &Slot, span: Span) -> bool {
        match span {
            Span::Now => slot.state.holds_now(),
            Span::Later => self.holds_later(model, slot),
        }
    }

    /// Whether `wanted` fits beside what holds memory in `span`, less `freed`
    ///
    /// `model` is what wants it, or `None` for a bare lease, which no exclusion
    /// names. Bare leases count as [`Self::bare_figures`] says, leaving out the
    /// waiter at `skip`.
    pub(super) fn fits(
        &self,
        model: Option<&ModelName>,
        wanted: Footprint,
        freed: &[ModelName],
        span: Span,
        skip: Option<(Priority, u64)>,
    ) -> bool {
        let others: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| {
                Some(*name) != model && !freed.contains(name) && self.holds_in(name, slot, span)
            })
            .collect();
        let excluded =
            model.is_some_and(|model| others.iter().any(|(name, _)| self.excluded(model, name)));
        let figures: Vec<_> = core::iter::once(wanted)
            .chain(others.iter().map(|(name, slot)| self.counted(name, slot)))
            .chain(self.bare_figures(span, skip))
            .collect();
        !excluded && self.config.host.fits(&figures)
    }

    /// Whether `a` and `b` may not be loaded together
    ///
    /// Beyond the config's exclusions, two models whose backends share a
    /// process are, counting the backend a model was loaded on while it holds
    /// memory as well as the one its config names now. Ollama backends count
    /// too: two models naming one ollama model share its runner and its unload.
    pub(super) fn excluded(&self, a: &ModelName, b: &ModelName) -> bool {
        let backends = |name: &ModelName| {
            let configured = self.config.models.get(name).map(|model| &model.backend);
            let running = self
                .slots
                .get(name)
                .filter(|slot| slot.state.holds_now())
                .and_then(|slot| slot.loaded_on.as_ref());
            [configured, running].into_iter().flatten()
        };
        let shared = a != b && backends(a).any(|x| backends(b).any(|y| x.same_process(y)));
        shared || self.config.excluded(a, b)
    }

    /// What `model` counts for against the host
    ///
    /// The larger of the figures it loaded with and its config's for its
    /// placement, in each resource. A model gone from the config, or running
    /// in a placement gone from it, counts at what it loaded with.
    pub(super) fn counted(&self, model: &ModelName, slot: &Slot) -> Footprint {
        let Some(configured) = self.config.models.get(model) else {
            return slot.footprint;
        };
        match &slot.placement {
            Some(placement) if !configured.placements.iter().any(|p| p.name == *placement) => {
                slot.footprint
            }
            placement => slot
                .footprint
                .larger(configured.footprint_at(placement.as_ref())),
        }
    }

    /// The fewest least recently used candidates whose eviction lets `model` fit at `wanted`
    ///
    /// Only the memory held once the models leaving are gone counts, so
    /// nothing is evicted for room an unload under way will free. Candidates
    /// are added oldest first until `model` fits, then any not needed are put
    /// back, newest first. The set is empty when that room is already coming.
    pub(super) fn eviction_set(
        &self,
        model: Option<&ModelName>,
        wanted: Footprint,
        candidates: Vec<ModelName>,
        skip: Option<(Priority, u64)>,
    ) -> Option<Vec<ModelName>> {
        let fits = |freed: &[ModelName]| self.fits(model, wanted, freed, Span::Later, skip);
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
    ///
    /// The first placement that fits now loads. Failing that, the first one
    /// whose room is already coming claims it, else the first one some
    /// eviction makes room for.
    pub(super) fn make_room(
        &mut self,
        now: Moment,
        model: ModelName,
        priority: Priority,
        out: &mut Vec<Action>,
    ) -> Reason {
        let options = self.options(&model);
        let fits_now = options
            .iter()
            .find(|(_, wanted)| self.may_load_as(&model, *wanted))
            .cloned();
        if let Some((placement, wanted)) = fits_now {
            self.place(&model, placement, wanted);
            self.start_load(now, &model, out);
            return Reason::Loading { model };
        }
        let order = self.in_the_way(now, Some(&model), priority);
        let free: Vec<_> = order
            .iter()
            .filter(|(guard, _)| *guard == Guard::Free)
            .map(|(_, name)| name.clone())
            .collect();
        let sets: Vec<_> = options
            .iter()
            .filter_map(|(placement, wanted)| {
                let set = self.eviction_set(Some(&model), *wanted, free.clone(), None)?;
                Some((placement.clone(), *wanted, set))
            })
            .collect();
        let chosen = sets
            .iter()
            .find(|(.., set)| set.is_empty())
            .or_else(|| sets.first())
            .cloned();
        if let Some((placement, wanted, set)) = chosen {
            self.place(&model, placement, wanted);
            self.evict(set, &Taker::Model(model.clone()), out);
            return Reason::Loading { model };
        }
        self.blocked(now, &Taker::Model(model), &options, &order, None)
    }

    /// Why `model` cannot have room, named by the guarded models in the way
    ///
    /// The models are those the search would take if guards were lifted, for
    /// the first placement whose set holds no held model, else the first with
    /// a set. Within that set a held one is named first, then a claim, then a
    /// grace period, so a refusal names the hardest block of the softest way.
    pub(super) fn blocked(
        &self,
        now: Moment,
        taker: &Taker,
        options: &[(Option<PlacementName>, Footprint)],
        order: &[(Guard, ModelName)],
        skip: Option<(Priority, u64)>,
    ) -> Reason {
        let names: Vec<ModelName> = order.iter().map(|(_, name)| name.clone()).collect();
        let sets: Vec<_> = options
            .iter()
            .filter_map(|(_, wanted)| {
                self.eviction_set(taker.model(), *wanted, names.clone(), skip)
            })
            .collect();
        let holds_held = |set: &Vec<ModelName>| {
            order
                .iter()
                .any(|(guard, name)| *guard == Guard::Held && set.contains(name))
        };
        let Some(set) = sets
            .iter()
            .find(|set| !holds_held(set))
            .or_else(|| sets.first())
        else {
            // Evicting every model would not make room, so bare leases hold it.
            return self
                .bare_reason(now, skip)
                .unwrap_or_else(|| Reason::Behind {
                    model: taker.clone(),
                });
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
            return Reason::Behind {
                model: Taker::Model(claimed),
            };
        }
        self.grace_reason(now, &guarded(Guard::Grace))
            .unwrap_or_else(|| Reason::Behind {
                model: taker.clone(),
            })
    }

    /// Every model holding or claiming room `model` needs later, in the order
    /// a search for room tries them
    ///
    /// Free and grace models go least recently used first, and a Reserved
    /// claim before a Loading one, since its waiter is the one ahead.
    pub(super) fn in_the_way(
        &self,
        now: Moment,
        model: Option<&ModelName>,
        priority: Priority,
    ) -> Vec<(Guard, ModelName)> {
        let mut found: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| Some(*name) != model && self.holds_later(name, slot))
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
                    _ => self.used_at(now, name),
                };
                (guard, age, name)
            })
            .collect();
        // An in-flight model sorts as used now. An interactive waiter may still
        // evict it; it drains, then unloads.
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
            Some(_) if self.in_flight_on(model) > 0 => now,
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
            || self.in_flight_on(model) > 0
            || self.kept(model)
            || self
                .waiters
                .values()
                .any(|waiter| waiter.model.as_ref() == Some(model));
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
    /// An Unloaded model nothing wants drops the retry it is owed.
    /// A bare lease no longer waiting drops its claim too.
    /// Returns whether any claim was dropped.
    pub(super) fn drop_unwanted_claims(&mut self) -> bool {
        let unwanted: Vec<_> = self
            .slots
            .iter()
            .filter(|(name, slot)| {
                matches!(slot.state, State::Reserved | State::Unloaded)
                    && !self.reloads(name)
                    && !self
                        .waiters
                        .values()
                        .any(|waiter| waiter.model.as_ref() == Some(*name))
            })
            .map(|(name, slot)| (name.clone(), slot.state))
            .collect();
        let mut dropped = false;
        for (model, state) in unwanted {
            if let Some(slot) = self.slots.get_mut(&model) {
                slot.state = State::Unloaded;
                slot.failed_once = false;
            }
            if state == State::Reserved {
                dropped = true;
                self.unclaim(&Taker::Model(model.clone()));
                self.refit(&model);
            }
        }
        dropped | self.drop_bare_claims()
    }

    /// Stops the evictions committed for `taker` from naming it
    pub(super) fn unclaim(&mut self, taker: &Taker) {
        for slot in self.slots.values_mut() {
            if slot.for_model.as_ref() == Some(taker) {
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
            out.push(Action::Load(model.clone()));
        }
    }

    /// Commits an eviction: `set` leaves, and its room is claimed for `taker`
    pub(super) fn evict(&mut self, set: Vec<ModelName>, taker: &Taker, out: &mut Vec<Action>) {
        for name in set {
            self.reclaim(&name, out);
            let drained = self.in_flight_on(&name) == 0;
            let Some(slot) = self.slots.get_mut(&name) else {
                continue;
            };
            slot.for_model = Some(taker.clone());
            if drained {
                slot.state = State::Unloading;
                out.push(Action::Unload(name));
            } else {
                slot.state = State::Evicting;
            }
        }
        match taker {
            Taker::Model(model) => {
                if let Some(slot) = self.slots.get_mut(model) {
                    slot.state = State::Reserved;
                }
            }
            Taker::Bare { lease, .. } => {
                self.claims.insert(*lease);
            }
        }
    }
}
