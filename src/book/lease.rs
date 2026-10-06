//! Leases: who holds which model, and when each hold ends.

use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};

use super::{Action, Book, Moment, Priority, Reason, State, WaiterId};
use crate::config::{ClientName, ModelName};

/// One lease, as the engine names it
// wire format: state.json holds it, so changing this is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct LeaseId(pub u64);

/// How a lease's holder shows it is still alive
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The holder keeps a stream open.
    Connection,
    /// The holder renews within every `ttl`.
    Heartbeat {
        /// How long a renewal lasts.
        ttl: Duration,
    },
}

/// What a client asks for when it asks for a lease
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseAsk {
    /// Names the lease once it is granted.
    pub lease: LeaseId,
    /// Who asks.
    pub client: ClientName,
    /// The model to hold.
    pub model: ModelName,
    /// Where it queues.
    pub priority: Priority,
    /// How long the holder expects to keep it, for estimates only.
    pub expected: Option<Duration>,
    /// How long it may wait before it is refused. Without one it waits on.
    pub max_wait: Option<Duration>,
    /// How its holder shows it is still alive.
    pub hold: Hold,
    /// What the holder says it is for.
    pub note: Option<String>,
}

/// How a lease ended
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Its holder released it.
    Released,
    /// Its holder did not renew it within its `ttl`.
    Expired,
    /// Its holder did not attach again within the reconnect window.
    Abandoned,
}

/// A granted lease, as the status reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseView {
    /// The lease.
    pub id: LeaseId,
    /// Who holds it.
    pub client: ClientName,
    /// The model it holds.
    pub model: ModelName,
    /// Where it queued, and where its model's reload queues after a crash.
    pub priority: Priority,
    /// When it was granted.
    pub since: Moment,
    /// When its holder expects to release it, if it said.
    pub expected_until: Option<Moment>,
    /// What the holder says it is for.
    pub note: Option<String>,
    /// How its holder shows it is still alive.
    pub hold: Hold,
    /// Whether a connection holder's stream is open. Heartbeat leases count as attached.
    pub attached: bool,
}

/// A granted lease
#[derive(Debug)]
pub(super) struct Lease {
    ask: LeaseAsk,
    since: Moment,
    renewed: Moment,
    detached: Option<Moment>,
    /// Whether its model loads again after a crash. A failed reload stops it.
    reload: bool,
}

impl Lease {
    /// A lease picked up after a restart, with every window counted from `now`
    ///
    /// No stream survives a restart, so a connection lease starts detached.
    pub fn restored(now: Moment, ask: LeaseAsk, since: Moment) -> Lease {
        let detached = match ask.hold {
            Hold::Connection => Some(now),
            Hold::Heartbeat { .. } => None,
        };
        Lease {
            ask,
            since,
            renewed: now,
            detached,
            reload: true,
        }
    }

    fn until(&self) -> Option<Moment> {
        self.ask.expected.map(|expected| self.since.plus(expected))
    }

    /// When it ends unless its holder renews or attaches first
    pub fn ends_at(&self, reconnect: Duration) -> Option<Moment> {
        match self.ask.hold {
            Hold::Heartbeat { ttl } => Some(self.renewed.plus(ttl)),
            Hold::Connection => self.detached.map(|at| at.plus(reconnect)),
        }
    }

    fn view(&self) -> LeaseView {
        LeaseView {
            id: self.ask.lease,
            client: self.ask.client.clone(),
            model: self.ask.model.clone(),
            priority: self.ask.priority,
            since: self.since,
            expected_until: self.until(),
            note: self.ask.note.clone(),
            hold: self.ask.hold,
            attached: self.detached.is_none(),
        }
    }
}

impl Book {
    /// The granted lease `id`, or `None` if it is not granted or has ended
    pub fn lease(&self, id: LeaseId) -> Option<LeaseView> {
        self.leases.get(&id).map(Lease::view)
    }

    /// Every granted lease, by id
    pub fn leases(&self) -> Vec<LeaseView> {
        self.leases.values().map(Lease::view).collect()
    }

    /// Whether a granted lease names `model`
    pub(super) fn held(&self, model: &ModelName) -> bool {
        self.leases.values().any(|lease| lease.ask.model == *model)
    }

    /// Whether a lease on `model` loads it again once its backend has exited
    pub(super) fn reloads(&self, model: &ModelName) -> bool {
        self.leases
            .values()
            .any(|lease| lease.reload && lease.ask.model == *model)
    }

    /// The reason naming the lease on any of `models` that ends last
    ///
    /// A lease that gave no expected end, or whose end has passed, counts
    /// as ending last, and its reason names no end.
    pub(super) fn held_reason(&self, now: Moment, models: &[ModelName]) -> Option<Reason> {
        let until = |lease: &Lease| lease.until().filter(|at| *at > now);
        let lease = self
            .leases
            .values()
            .filter(|lease| models.contains(&lease.ask.model))
            .max_by_key(|lease| (until(lease).is_none(), until(lease), lease.ask.lease))?;
        Some(Reason::Held {
            model: lease.ask.model.clone(),
            client: lease.ask.client.clone(),
            lease: lease.ask.lease,
            since: lease.since,
            until: until(lease),
        })
    }

    /// Grants `ask` on its Loaded model, which is held from now on
    ///
    /// An ask naming a live lease's id fails, and the live lease stands.
    pub(super) fn grant(
        &mut self,
        now: Moment,
        waiter: WaiterId,
        ask: LeaseAsk,
        out: &mut Vec<Action>,
    ) {
        let lease = ask.lease;
        if self.leases.contains_key(&lease) {
            out.push(Action::Fail {
                waiter,
                error: format!("lease {} is already granted", lease.0),
            });
            return;
        }
        let granted = Lease {
            ask,
            since: now,
            renewed: now,
            detached: None,
            reload: true,
        };
        self.leases.insert(lease, granted);
        out.push(Action::Grant { waiter, lease });
        out.push(Action::Persist);
    }

    pub(super) fn renew(&mut self, now: Moment, id: LeaseId) {
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.renewed = now;
        }
    }

    /// Starts a connection lease's reconnect window, unless one is already running
    pub(super) fn detach(&mut self, now: Moment, id: LeaseId) {
        if let Some(lease) = self.leases.get_mut(&id)
            && lease.ask.hold == Hold::Connection
        {
            lease.detached.get_or_insert(now);
        }
    }

    pub(super) fn attach(&mut self, id: LeaseId) {
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.detached = None;
        }
    }

    /// Ends the lease
    pub(super) fn end(&mut self, id: LeaseId, why: Ended, out: &mut Vec<Action>) {
        if self.leases.remove(&id).is_none() {
            return;
        }
        out.push(Action::LeaseEnded { lease: id, why });
        out.push(Action::Persist);
    }

    /// Ends every lease whose holder missed its renewal or reconnect window
    pub(super) fn expire(&mut self, now: Moment, out: &mut Vec<Action>) {
        let ended: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, lease)| {
                lease
                    .ends_at(self.config.reconnect)
                    .is_some_and(|at| at <= now)
            })
            .map(|(id, lease)| match lease.ask.hold {
                Hold::Heartbeat { .. } => (*id, Ended::Expired),
                Hold::Connection => (*id, Ended::Abandoned),
            })
            .collect();
        for (id, why) in ended {
            self.end(id, why, out);
        }
    }

    /// Sets whether the leases on `model` load it again after a crash
    ///
    /// A load that fails twice stops it, so a broken backend is not retried
    /// without end. A load that succeeds turns it back on.
    pub(super) fn reload_on_crash(&mut self, model: &ModelName, reload: bool) {
        for lease in self.leases.values_mut() {
            if lease.ask.model == *model {
                lease.reload = reload;
            }
        }
    }

    /// Loads again every held model whose backend exited, as a waiter would
    ///
    /// Each goes ahead of the queue, at its leases' highest priority, and
    /// its lease is not granted again. A grace period that blocks one is
    /// kept, so a Tick comes when it ends.
    pub(super) fn reload_held(&mut self, now: Moment, out: &mut Vec<Action>) {
        let mut crashed: BTreeMap<ModelName, Priority> = BTreeMap::new();
        for lease in self.leases.values() {
            if lease.reload && self.state(&lease.ask.model) == Some(State::Unloaded) {
                let priority = crashed
                    .entry(lease.ask.model.clone())
                    .or_insert(Priority::Batch);
                *priority = (*priority).min(lease.ask.priority);
            }
        }
        self.reload_grace.clear();
        for (model, priority) in crashed {
            if self.state(&model) != Some(State::Unloaded) {
                continue;
            }
            if let Reason::Grace { until, .. } = self.make_room(now, model, priority, out) {
                self.reload_grace.push(until);
            }
        }
    }
}
