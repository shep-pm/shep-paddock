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
    /// Keeps its model loaded without holding it: the model may be evicted,
    /// which ends the lease.
    pub reclaimable: bool,
    /// Ends it once its holder has neither used its model through the dog
    /// nor sent a note for this long.
    pub release_if_idle: Option<Duration>,
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
    /// Its model was evicted or its backend exited, and it was reclaimable.
    Reclaimed,
    /// Its holder neither used its model through the dog nor sent a note
    /// for `after`, and it asked to be released then.
    Idle {
        /// How long it asked to sit idle before it ends.
        after: Duration,
    },
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
    /// Where it queued, and where its model's reload queues after a crash, for a held lease.
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
    /// Whether it keeps its model loaded without holding it.
    pub reclaimable: bool,
    /// The later of its grant, its holder's last request for its model, and its last note.
    pub last_activity: Moment,
    /// Whether a request of its holder's for its model is in flight.
    pub in_use: bool,
    /// How long it may sit idle before it ends, if it asked.
    pub release_if_idle: Option<Duration>,
}

/// A granted lease
#[derive(Debug)]
pub(super) struct Lease {
    pub(super) ask: LeaseAsk,
    since: Moment,
    pub(super) renewed: Moment,
    /// When its holder last used its model through the dog, or sent a note.
    pub(super) last_activity: Moment,
    detached: Option<Moment>,
    /// Whether its model loads again after a crash. A failed reload stops it.
    reload: bool,
}

impl Lease {
    /// A lease picked up after a restart, with every window counted from `now`
    ///
    /// No stream survives a restart, so a connection lease starts detached.
    /// Its idle clock runs from its saved activity, or from `now` without one.
    pub fn restored(
        now: Moment,
        ask: LeaseAsk,
        since: Moment,
        last_activity: Option<Moment>,
    ) -> Lease {
        let detached = match ask.hold {
            Hold::Connection => Some(now),
            Hold::Heartbeat { .. } => None,
        };
        Lease {
            ask,
            since,
            renewed: now,
            last_activity: last_activity.unwrap_or(now),
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

    /// How it ends when its holder misses its window
    fn missed(&self) -> Ended {
        match self.ask.hold {
            Hold::Heartbeat { .. } => Ended::Expired,
            Hold::Connection => Ended::Abandoned,
        }
    }

    fn view(&self, in_use: bool) -> LeaseView {
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
            reclaimable: self.ask.reclaimable,
            last_activity: self.last_activity,
            in_use,
            release_if_idle: self.ask.release_if_idle,
        }
    }
}

impl Book {
    /// The granted lease `id`, or `None` if it is not granted or has ended
    pub fn lease(&self, id: LeaseId) -> Option<LeaseView> {
        self.leases
            .get(&id)
            .map(|lease| lease.view(self.in_use(lease)))
    }

    /// Every granted lease, by id
    pub fn leases(&self) -> Vec<LeaseView> {
        self.leases
            .values()
            .map(|lease| lease.view(self.in_use(lease)))
            .collect()
    }

    /// Whether a lease that is not reclaimable names `model`
    pub(super) fn held(&self, model: &ModelName) -> bool {
        self.leases
            .values()
            .any(|lease| !lease.ask.reclaimable && lease.ask.model == *model)
    }

    /// Whether any lease names `model`, held or reclaimable, so it is not unloaded for idleness
    pub(super) fn kept(&self, model: &ModelName) -> bool {
        self.leases.values().any(|lease| lease.ask.model == *model)
    }

    /// Whether a held lease on `model` loads it again once its backend has exited
    pub(super) fn reloads(&self, model: &ModelName) -> bool {
        self.leases
            .values()
            .any(|lease| lease.reload && !lease.ask.reclaimable && lease.ask.model == *model)
    }

    /// The reason naming the held lease on any of `models` that ends last
    ///
    /// A lease that gave no expected end, or whose end has passed, counts
    /// as ending last, and its reason names no end.
    pub(super) fn held_reason(&self, now: Moment, models: &[ModelName]) -> Option<Reason> {
        let until = |lease: &Lease| lease.until().filter(|at| *at > now);
        let lease = self
            .leases
            .values()
            .filter(|lease| !lease.ask.reclaimable && models.contains(&lease.ask.model))
            .max_by_key(|lease| (until(lease).is_none(), until(lease), lease.ask.lease))?;
        Some(Reason::Held {
            model: lease.ask.model.clone(),
            client: lease.ask.client.clone(),
            lease: lease.ask.lease,
            since: lease.since,
            until: until(lease),
            idle_since: (!self.in_use(lease)).then_some(lease.last_activity),
        })
    }

    /// Grants `ask` on its Loaded model, which is held from now on unless the ask is reclaimable
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
            last_activity: now,
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

    /// Ends every reclaimable lease on `model`, which is leaving: evicted, or its backend exited
    pub(super) fn reclaim(&mut self, model: &ModelName, out: &mut Vec<Action>) {
        let reclaimed: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.ask.reclaimable && lease.ask.model == *model)
            .map(|(id, _)| *id)
            .collect();
        for id in reclaimed {
            self.end(id, Ended::Reclaimed, out);
        }
    }

    /// Ends every lease whose holder missed its window, or that sat idle as long as it asked
    ///
    /// A lease past two ends ends for the earlier one.
    pub(super) fn expire(&mut self, now: Moment, out: &mut Vec<Action>) {
        let ended: Vec<_> = self
            .leases
            .iter()
            .filter_map(|(id, lease)| {
                let missed = lease
                    .ends_at(self.config.reconnect)
                    .map(|at| (at, lease.missed()));
                [missed, self.idle_ends(lease)]
                    .into_iter()
                    .flatten()
                    .filter(|(at, _)| *at <= now)
                    .min_by_key(|(at, _)| *at)
                    .map(|(_, why)| (*id, why))
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
            if lease.reload
                && !lease.ask.reclaimable
                && self.state(&lease.ask.model) == Some(State::Unloaded)
            {
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
