//! Who waits, why, until when, and when they are refused.

use std::time::Duration;

use super::{
    Action, Book, LeaseAsk, LeaseId, Moment, Priority, State, WaiterId,
    snapshot::{WaiterKind, WaiterView},
};
use crate::config::{ClientName, ModelName};

// The spec's estimate for a model that has never loaded.
const FIRST_LOAD: Duration = Duration::from_secs(60);

/// Why a waiter cannot be served yet
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Its model is loading, or will load once the memory it needs is free.
    Loading {
        /// The waiter's model.
        model: ModelName,
    },
    /// Its model is being evicted to make room for another.
    Evicting {
        /// The waiter's model.
        model: ModelName,
        /// The model the room goes to.
        for_model: ModelName,
    },
    /// Its model is unloading for a reason other than an eviction.
    Draining {
        /// The waiter's model.
        model: ModelName,
    },
    /// Making room needs a batch waiter to evict a model used too recently.
    Grace {
        /// The model in its grace period.
        model: ModelName,
        /// When its grace period ends.
        until: Moment,
    },
    /// Making room needs a model a lease holds.
    Held {
        /// The held model.
        model: ModelName,
        /// Who holds it.
        client: ClientName,
        /// The lease that holds it.
        lease: LeaseId,
        /// When the lease was granted.
        since: Moment,
        /// When the lease expects to end, if it said.
        until: Option<Moment>,
    },
    /// Making room needs a model still loading, or claimed by another waiter.
    Behind {
        /// The model loading or claimed.
        model: ModelName,
    },
}

/// Why a waiter was turned away, and when to try again
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// What it was waiting on.
    pub reason: Reason,
    /// How long until it could be served, when that can be said.
    pub retry_after: Option<Duration>,
}

/// A request or lease that cannot be served yet
#[derive(Debug)]
pub(super) struct Waiter {
    pub id: WaiterId,
    client: ClientName,
    pub model: ModelName,
    since: Moment,
    /// When it is refused if still waiting. A lease with no cap has none.
    pub deadline: Option<Moment>,
    /// What it asks for, when it is a lease.
    pub lease: Option<LeaseAsk>,
    told: Option<(Reason, Option<Moment>)>,
}

impl Waiter {
    pub fn request(
        now: Moment,
        id: WaiterId,
        client: ClientName,
        model: ModelName,
        max_wait: Duration,
    ) -> Self {
        Self {
            id,
            client,
            model,
            since: now,
            deadline: Some(now.plus(max_wait)),
            lease: None,
            told: None,
        }
    }

    pub fn lease(now: Moment, id: WaiterId, ask: LeaseAsk) -> Self {
        Self {
            id,
            client: ask.client.clone(),
            model: ask.model.clone(),
            since: now,
            deadline: ask.max_wait.map(|cap| now.plus(cap)),
            lease: Some(ask),
            told: None,
        }
    }

    /// How the status shows it, with the reason and estimate it was last told
    pub fn view(&self, priority: Priority) -> WaiterView {
        let (reason, estimate) = self.told.clone().unzip();
        WaiterView {
            client: self.client.clone(),
            model: self.model.clone(),
            kind: match self.lease {
                Some(_) => WaiterKind::Lease,
                None => WaiterKind::Request,
            },
            priority,
            since: self.since,
            reason,
            estimate: estimate.flatten(),
        }
    }

    /// When the grace period it waits out ends, if it waits on one
    pub fn grace_ends(&self) -> Option<Moment> {
        match self.told {
            Some((Reason::Grace { until, .. }, _)) => Some(until),
            _ => None,
        }
    }

    /// The refusal it gets now, or `None` while it may keep waiting
    fn refusal(&self, now: Moment, reason: &Reason, estimate: Option<Moment>) -> Option<Refusal> {
        let deadline = self.deadline?;
        let endless = matches!(reason, Reason::Held { until: None, .. });
        let late = estimate.is_some_and(|at| at > deadline);
        (now >= deadline || endless || late).then(|| Refusal {
            reason: reason.clone(),
            retry_after: estimate.filter(|at| *at > now).map(|at| at.since(now)),
        })
    }

    /// The `Waiting` action, or `None` when it repeats the last one told
    fn tell(&mut self, reason: Reason, estimate: Option<Moment>) -> Option<Action> {
        let told = (reason, estimate);
        if self.told.as_ref() == Some(&told) {
            return None;
        }
        let (reason, estimate) = told.clone();
        self.told = Some(told);
        Some(Action::Waiting {
            waiter: self.id,
            reason,
            estimate,
        })
    }
}

impl Book {
    /// Serves every waiter that can be served, in order, and answers the rest
    ///
    /// A Reserved model nothing waits for first drops its claim. Waiters on
    /// a Loaded model are admitted before anything is evicted, so a model is
    /// never evicted from under a waiter ready to use it. Crashed held models
    /// then claim room ahead of the queue. Reserved models get freed room
    /// before the walk and again after it. Estimates are read once the
    /// walk's loads have started.
    pub(super) fn reconsider(&mut self, now: Moment, out: &mut Vec<Action>) {
        self.drop_unwanted_claims();
        self.load_reserved(now, out);
        let ready: Vec<_> = self
            .waiters
            .iter()
            .filter(|(_, waiter)| self.state(&waiter.model) == Some(State::Loaded))
            .map(|(key, _)| *key)
            .collect();
        for key in ready {
            if let Some(waiter) = self.waiters.remove(&key) {
                self.admit(now, waiter, out);
            }
        }
        self.reload_held(now, out);
        let keys: Vec<_> = self.waiters.keys().copied().collect();
        let mut reasons = Vec::new();
        for key in keys {
            if let Some(reason) = self.serve(now, key, out) {
                reasons.push((key, reason));
            }
        }
        self.load_reserved(now, out);
        for (key, reason) in reasons {
            self.answer(now, key, reason, out);
        }
    }

    /// Admits the waiter, or makes room for it and says why it still waits
    fn serve(
        &mut self,
        now: Moment,
        key: (Priority, u64),
        out: &mut Vec<Action>,
    ) -> Option<Reason> {
        let model = self.waiters.get(&key)?.model.clone();
        let slot = self.slots.get(&model)?;
        Some(match slot.state {
            State::Loaded => {
                if let Some(waiter) = self.waiters.remove(&key) {
                    self.admit(now, waiter, out);
                }
                return None;
            }
            State::Reserved | State::Loading => Reason::Loading { model },
            State::Evicting | State::Unloading => match slot.for_model.clone() {
                Some(for_model) => Reason::Evicting { model, for_model },
                None => Reason::Draining { model },
            },
            State::Unloaded => self.make_room(now, model, key.0, out),
        })
    }

    /// Refuses the waiter, or tells it why it waits when that has changed
    fn answer(&mut self, now: Moment, key: (Priority, u64), reason: Reason, out: &mut Vec<Action>) {
        let Some(waiter) = self.waiters.get(&key) else {
            return;
        };
        let estimate = self.estimate(&waiter.model, &reason);
        if let Some(refusal) = waiter.refusal(now, &reason, estimate) {
            out.push(Action::Refuse {
                waiter: waiter.id,
                refusal,
            });
            self.waiters.remove(&key);
        } else if let Some(waiter) = self.waiters.get_mut(&key) {
            out.extend(waiter.tell(reason, estimate));
        }
    }

    /// When a waiter on `wanted` held up by `reason` should be served
    fn estimate(&self, wanted: &ModelName, reason: &Reason) -> Option<Moment> {
        match reason {
            Reason::Loading { model } | Reason::Behind { model } => self.loaded_by(model),
            Reason::Held { until, .. } => *until,
            Reason::Grace { until, .. } => Some(until.plus(self.load_time(wanted))),
            Reason::Evicting { .. } | Reason::Draining { .. } => None,
        }
    }

    /// When `model` should finish loading, if it is loading
    fn loaded_by(&self, model: &ModelName) -> Option<Moment> {
        let slot = self.slots.get(model)?;
        (slot.state == State::Loading).then(|| slot.load_started.plus(self.load_time(model)))
    }

    /// How long `model` took to load last time
    fn load_time(&self, model: &ModelName) -> Duration {
        self.slots
            .get(model)
            .and_then(|slot| slot.load_took)
            .unwrap_or(FIRST_LOAD)
    }
}
