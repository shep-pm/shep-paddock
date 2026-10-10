//! Taking a new config, and picking up leases and loaded models after a restart.

use std::sync::Arc;

use super::{
    Action, Book, Ended, LeaseAsk, LeaseId, Moment, NEVER_FITS, Slot, State, Taker, Waiter,
    lease::Lease,
};
use crate::{
    config::{Config, Model, ModelName, PlacementName},
    footprint::Footprint,
};

/// A model found holding memory at a restart
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// The model, or a stand-in's name.
    pub model: ModelName,
    /// What it counts for.
    pub footprint: Footprint,
    /// The placement it was loaded in, when that is known.
    pub placement: Option<PlacementName>,
    /// Loaded by something other than the dog.
    pub stray: bool,
}

/// A lease saved before a restart
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoredLease {
    /// What was granted.
    pub ask: LeaseAsk,
    /// When it was granted.
    pub since: Moment,
    /// When its holder last used it, if that was saved.
    pub last_activity: Option<Moment>,
}

impl Book {
    /// Applies `config` to later decisions
    ///
    /// Nothing loaded unloads because its figures changed. Until it unloads, a model counts at the
    /// larger of its old and new figures. A model gone from the config keeps its leases and unloads
    /// once nothing names it. Until then no model on its backend loads. Its waiters and any load
    /// under way fail. Evictions committed for it stand but stop naming it. Every Reserved model
    /// claims its room again under the new figures. A model back in the config counts the requests
    /// still in flight on it. A bare waiter the host can no longer hold fails.
    pub fn reconfigure(&mut self, now: Moment, config: Arc<Config>) -> Vec<Action> {
        let mut out = Vec::new();
        self.expire(now, &mut out);
        self.config = config;
        for model in self.config.models.values() {
            self.slots
                .entry(model.name.clone())
                .or_insert_with(|| Slot::new(model.footprint));
        }
        let names: Vec<_> = self.slots.keys().cloned().collect();
        self.claims.clear();
        for name in &names {
            if let Some(slot) = self.slots.get_mut(name)
                && slot.state == State::Reserved
            {
                slot.state = State::Unloaded;
            }
            if !self.config.models.contains_key(name) {
                self.unclaim(&Taker::Model(name.clone()));
            }
            self.refit(name);
        }
        let config = Arc::clone(&self.config);
        let removed = |waiter: &Waiter| {
            if let Some(footprint) = waiter.lease.as_ref().and_then(LeaseAsk::bare)
                && !config.host.ever_fits(&footprint)
            {
                return Some(NEVER_FITS.to_owned());
            }
            waiter
                .model
                .as_ref()
                .filter(|model| !config.models.contains_key(*model))
                .map(|model| format!("{model} was removed from the config"))
        };
        self.fail_waiters(now, removed, &mut out);
        self.settle(now, out)
    }

    /// Picks up the leases and loaded models a restart left, before any event
    ///
    /// Each lease's renewal and reconnect windows start at `now`. A lease
    /// whose id is already live is skipped, and one already past its idle
    /// end ends before it can load its model. A reclaimable lease whose model
    /// was not found loaded ends reclaimed. A loaded model counts at the
    /// footprint given, or more if its placement's figures are larger. One
    /// with no config entry and no lease is unknown: reclaimable, and never
    /// served. Each of `stand_ins` excludes the models its backend serves, and
    /// one named for a configured model is the backend it was found on.
    pub fn restore(
        &mut self,
        now: Moment,
        loaded: Vec<Found>,
        stand_ins: &[Model],
        leases: Vec<RestoredLease>,
    ) -> Vec<Action> {
        for restored in leases {
            self.leases.entry(restored.ask.lease).or_insert_with(|| {
                Lease::restored(now, restored.ask, restored.since, restored.last_activity)
            });
        }
        let mut out = Vec::new();
        self.expire(now, &mut out);
        for Found {
            model,
            footprint,
            placement,
            stray,
        } in loaded
        {
            let configured = self.config.models.get(&model);
            let unknown = configured.is_none() && !self.kept(&model);
            let backend = stand_ins
                .iter()
                .find(|stand_in| stand_in.name == model)
                .or(configured)
                .map(|found| found.backend.clone());
            let slot = self
                .slots
                .entry(model)
                .or_insert_with(|| Slot::new(footprint));
            slot.state = State::Loaded;
            slot.footprint = footprint;
            slot.placement = placement;
            slot.last_used = now;
            slot.unknown = unknown;
            slot.stray = stray;
            slot.loaded_on = backend;
        }
        let gone: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, lease)| {
                lease.ask.reclaimable
                    && lease
                        .ask
                        .model()
                        .is_none_or(|model| self.state(model) != Some(State::Loaded))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.end(id, Ended::Reclaimed, &mut out);
        }
        self.settle(now, out)
    }

    /// The highest granted lease id, so the engine numbers new leases past it
    pub fn max_lease_id(&self) -> Option<LeaseId> {
        self.leases.keys().next_back().copied()
    }
}
