//! What the status reports: each model, lease and waiter, and recent load failures.

use super::{Book, Moment, Priority, Reason, State, lease::LeaseView};
use crate::{
    config::{ClientName, ModelName, PlacementName},
    footprint::Footprint,
};

/// The book at one moment, for the status endpoint
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Every model the book knows, by name.
    pub models: Vec<ModelView>,
    /// Every granted lease, by id.
    pub leases: Vec<LeaseView>,
    /// Every waiter, in the order they are served.
    pub waiters: Vec<WaiterView>,
    /// The latest failed loads and silent backends, oldest first.
    pub errors: Vec<LoadError>,
    /// What every model not Unloaded counts for against the host, summed.
    pub declared: Footprint,
}

/// One model, as the status reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelView {
    /// The model.
    pub name: ModelName,
    /// Where it is between unloaded and loaded.
    pub state: State,
    /// Requests forwarded to it and not yet finished.
    pub in_flight: u32,
    /// When it was last used, where a request in flight is use now.
    pub last_used: Moment,
    /// The clients whose held leases name it, by name.
    pub held_by: Vec<ClientName>,
    /// Found loaded at a restart with no config entry and no lease.
    pub unknown: bool,
    /// The placement it claimed room in or loaded in, or `None` for a model without
    /// placements and while Unloaded.
    pub placement: Option<PlacementName>,
    /// What it counts for against the host now.
    pub footprint: Footprint,
}

/// Whether a waiter is a request or a lease
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterKind {
    /// A request waiting to be forwarded.
    Request,
    /// A lease waiting to be granted.
    Lease,
}

/// One waiter, as the status reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaiterView {
    /// Who asked.
    pub client: ClientName,
    /// The model it waits for.
    pub model: ModelName,
    /// A request or a lease.
    pub kind: WaiterKind,
    /// Where it queues.
    pub priority: Priority,
    /// When it arrived.
    pub since: Moment,
    /// Why it waits, as it was last told.
    pub reason: Option<Reason>,
    /// When it should be served, as it was last told.
    pub estimate: Option<Moment>,
}

/// A load that failed twice, or a backend that could not be asked at start
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadError {
    /// The model.
    pub model: ModelName,
    /// When the second attempt failed, or when the engine started without an answer from the
    /// backend.
    pub at: Moment,
    /// What the backend said, or why it could not be asked.
    pub error: String,
}

impl Book {
    /// What the status reports at `now`
    pub fn snapshot(&self, now: Moment) -> Snapshot {
        let leases = self.leases();
        let models = self
            .slots
            .iter()
            .map(|(name, slot)| {
                let mut held_by: Vec<_> = leases
                    .iter()
                    .filter(|lease| !lease.reclaimable && lease.model == *name)
                    .map(|lease| lease.client.clone())
                    .collect();
                held_by.sort();
                held_by.dedup();
                ModelView {
                    name: name.clone(),
                    state: slot.state,
                    in_flight: self.in_flight_on(name),
                    last_used: self.used_at(now, name),
                    held_by,
                    unknown: slot.unknown,
                    placement: slot.placement.clone(),
                    footprint: self.counted(name, slot),
                }
            })
            .collect();
        let waiters = self
            .waiters
            .iter()
            .map(|((priority, _), waiter)| waiter.view(*priority))
            .collect();
        let holding: Vec<_> = self
            .slots
            .iter()
            .filter(|(_, slot)| slot.state != State::Unloaded)
            .map(|(name, slot)| self.counted(name, slot))
            .collect();
        Snapshot {
            models,
            leases,
            waiters,
            errors: self.errors.iter().cloned().collect(),
            declared: self.config.host.declared(&holding),
        }
    }
}
