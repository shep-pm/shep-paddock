//! Leases: who holds which model, and when each hold ends.

use std::time::Duration;

use super::{Action, Book, Moment, Priority, Reason, WaiterId};
use crate::config::{ClientName, ModelName};

/// One lease, as the engine names it
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
}

impl Lease {
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

    /// The reason naming the lease on any of `models` that ends last
    ///
    /// A lease that gave no expected end counts as ending last.
    pub(super) fn held_reason(&self, models: &[ModelName]) -> Option<Reason> {
        let lease = self
            .leases
            .values()
            .filter(|lease| models.contains(&lease.ask.model))
            .max_by_key(|lease| (lease.until().is_none(), lease.until(), lease.ask.lease))?;
        Some(Reason::Held {
            model: lease.ask.model.clone(),
            client: lease.ask.client.clone(),
            lease: lease.ask.lease,
            since: lease.since,
            until: lease.until(),
        })
    }

    /// Grants `ask` on its Loaded model, which is held from now on
    pub(super) fn grant(
        &mut self,
        now: Moment,
        waiter: WaiterId,
        ask: LeaseAsk,
        out: &mut Vec<Action>,
    ) {
        if let Some(slot) = self.slots.get_mut(&ask.model) {
            slot.last_used = now;
        }
        let lease = ask.lease;
        let granted = Lease {
            ask,
            since: now,
            renewed: now,
            detached: None,
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

    /// Starts the reconnect window, unless one is already running
    pub(super) fn detach(&mut self, now: Moment, id: LeaseId) {
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.detached.get_or_insert(now);
        }
    }

    pub(super) fn attach(&mut self, id: LeaseId) {
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.detached = None;
        }
    }

    /// Ends the lease, which counts as its holder's last use of the model
    pub(super) fn end(&mut self, now: Moment, id: LeaseId, why: Ended, out: &mut Vec<Action>) {
        let Some(lease) = self.leases.remove(&id) else {
            return;
        };
        if let Some(slot) = self.slots.get_mut(&lease.ask.model) {
            slot.last_used = now;
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
            self.end(now, id, why, out);
        }
    }
}
