//! Taking a new config, and picking up leases and loaded models after a restart.

use std::sync::Arc;

use super::{Action, Book, LeaseAsk, LeaseId, Moment, Slot, State, lease::Lease};
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
}

/// A lease saved before a restart
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoredLease {
    /// What was granted.
    pub ask: LeaseAsk,
    /// When it was granted.
    pub since: Moment,
}

impl Book {
    /// Applies `config` to later decisions
    ///
    /// Nothing loaded unloads because its figures changed. Until it unloads,
    /// a model counts at the larger of its old and new figures. A model gone
    /// from the config keeps its leases and unloads once nothing names it.
    /// Until then no model on its backend loads. Its waiters and any load under
    /// way fail. Evictions committed for it stand but stop naming it. Every
    /// Reserved model claims its room again under the new figures.
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
        for name in &names {
            if let Some(slot) = self.slots.get_mut(name)
                && slot.state == State::Reserved
            {
                slot.state = State::Unloaded;
            }
            if !self.config.models.contains_key(name) {
                self.unclaim(name);
            }
            self.refit(name);
        }
        let config = Arc::clone(&self.config);
        self.waiters.retain(|_, waiter| {
            if config.models.contains_key(&waiter.model) {
                return true;
            }
            out.push(Action::Fail {
                waiter: waiter.id,
                error: format!("{} was removed from the config", waiter.model),
            });
            false
        });
        self.settle(now, out)
    }

    /// Picks up the leases and loaded models a restart left, before any event
    ///
    /// Each lease's renewal and reconnect windows start at `now`. A lease
    /// whose id is already live is skipped. A loaded model counts at the
    /// footprint given, or more if its placement's figures are larger. One
    /// with no config entry and no lease is unknown: reclaimable, and never
    /// served. Each of `stand_ins` excludes the models its backend serves.
    pub fn restore(
        &mut self,
        now: Moment,
        loaded: Vec<Found>,
        stand_ins: &[Model],
        leases: Vec<RestoredLease>,
    ) -> Vec<Action> {
        for restored in leases {
            self.leases
                .entry(restored.ask.lease)
                .or_insert_with(|| Lease::restored(now, restored.ask, restored.since));
        }
        for Found {
            model,
            footprint,
            placement,
        } in loaded
        {
            let configured = self.config.models.get(&model);
            let unknown = configured.is_none() && !self.held(&model);
            let backend = configured
                .or_else(|| stand_ins.iter().find(|stand_in| stand_in.name == model))
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
            slot.loaded_on = backend;
        }
        self.settle(now, Vec::new())
    }

    /// The highest granted lease id, so the engine numbers new leases past it
    pub fn max_lease_id(&self) -> Option<LeaseId> {
        self.leases.keys().next_back().copied()
    }
}
