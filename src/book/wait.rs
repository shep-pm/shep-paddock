//! Who waits, why, until when, and when they are refused.

use core::fmt;
use std::time::Duration;

use super::{
    Action, Book, LeaseAsk, LeaseId, Leased, Moment, Priority, State, WaiterId,
    snapshot::{WaiterKind, WaiterView},
};
use crate::{
    config::{ClientName, ModelName},
    footprint::Footprint,
};

// The spec's estimate for a model that has never loaded.
const FIRST_LOAD: Duration = Duration::from_secs(60);

/// What holds or claims room on the host: a model, or a bare lease's memory
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Taker {
    /// A model.
    Model(ModelName),
    /// A bare lease.
    Bare {
        /// The lease.
        lease: LeaseId,
        /// Who holds it, or asks for it.
        client: ClientName,
        /// What it declares.
        footprint: Footprint,
    },
}

impl Taker {
    /// What `ask` takes room for
    pub(super) fn of(ask: &LeaseAsk) -> Taker {
        match &ask.leased {
            Leased::Model(model) => Taker::Model(model.clone()),
            Leased::Bare { footprint, .. } => Taker::Bare {
                lease: ask.lease,
                client: ask.client.clone(),
                footprint: *footprint,
            },
        }
    }

    /// The model, or `None` for a bare lease
    pub fn model(&self) -> Option<&ModelName> {
        match self {
            Self::Model(model) => Some(model),
            Self::Bare { .. } => None,
        }
    }
}

impl From<ModelName> for Taker {
    fn from(model: ModelName) -> Self {
        Self::Model(model)
    }
}

/// A model by its name, a bare lease as `lease L12 of bench-01 (12G VRAM, 4G RAM)`
impl fmt::Display for Taker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(model) => write!(f, "{model}"),
            Self::Bare {
                lease,
                client,
                footprint,
            } => write!(f, "lease {lease} of {client} ({footprint})"),
        }
    }
}

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
        /// What the room goes to.
        for_model: Taker,
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
        /// What is held: a model, or a bare lease's memory.
        model: Taker,
        /// Who holds it.
        client: ClientName,
        /// The lease that holds it.
        lease: LeaseId,
        /// When the lease was granted.
        since: Moment,
        /// When the lease expects to end, if it said.
        until: Option<Moment>,
        /// When its lease was last used, or `None` while a request of its
        /// holder's is in flight or queued.
        idle_since: Option<Moment>,
    },
    /// Making room needs a model still loading, or room another waiter claimed.
    Behind {
        /// What is loading, or what claimed the room.
        model: Taker,
    },
    /// Leases take every turn its model's backend serves, while it is loaded or loads again.
    Turn {
        /// The waiter's model.
        model: ModelName,
        /// Who takes each turn, in lease order.
        holders: Vec<TurnHolder>,
        /// How many leases wait for a turn on the model ahead of it.
        ahead: usize,
    },
}

/// A lease taking one of its model's turns, as a waiter for one is told
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnHolder {
    /// Who holds it.
    pub client: ClientName,
    /// What the holder says it is for.
    pub note: Option<String>,
    /// When it expects to end, if it said and that has not passed.
    pub until: Option<Moment>,
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
    pub client: ClientName,
    /// The model it waits for, or `None` for a bare lease.
    pub model: Option<ModelName>,
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
            model: Some(model),
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
            model: ask.model().cloned(),
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
    ///
    /// A hold with no expected end refuses a capped waiter at once. A turn
    /// whose holders gave no end does not: it frees once enough holders end.
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
    ///
    /// A change in a holding lease's idle time, or in a turn holder's note,
    /// is kept for the status but not told, since its holder changes it
    /// with every request or note.
    fn tell(&mut self, reason: Reason, estimate: Option<Moment>) -> Option<Action> {
        let repeats = self.told.as_ref().is_some_and(|(told, told_estimate)| {
            *told_estimate == estimate && told.steady() == reason.steady()
        });
        self.told = Some((reason.clone(), estimate));
        (!repeats).then_some(Action::Waiting {
            waiter: self.id,
            reason,
            estimate,
        })
    }
}

impl Reason {
    /// The reason with no idle time or holder notes, to compare what changed apart from them
    fn steady(&self) -> Reason {
        let mut reason = self.clone();
        match &mut reason {
            Reason::Held { idle_since, .. } => *idle_since = None,
            Reason::Turn { holders, .. } => {
                holders.iter_mut().for_each(|holder| holder.note = None)
            }
            _ => {}
        }
        reason
    }
}

impl Book {
    /// Serves every waiter that can be served, in order, and answers the rest
    ///
    /// A Reserved model nothing waits for drops its claim before the walk.
    /// One whose last waiter the walk refused drops it after, and the walk
    /// runs again, since the room it claimed may serve a waiter behind it.
    pub(super) fn reconsider(&mut self, now: Moment, out: &mut Vec<Action>) {
        self.drop_unwanted_claims();
        loop {
            self.walk(now, out);
            if !self.drop_unwanted_claims() {
                break;
            }
        }
    }

    /// One pass over the waiters
    ///
    /// Waiters on a Loaded model are admitted before any eviction, so no
    /// model is evicted from under a ready waiter. Crashed held models then
    /// claim room ahead of the queue. Reserved models get freed room before
    /// the walk and again after it. Estimates are read once the walk's loads
    /// have started.
    fn walk(&mut self, now: Moment, out: &mut Vec<Action>) {
        self.load_reserved(now, out);
        let ready: Vec<_> = self
            .waiters
            .iter()
            .filter(|(_, waiter)| {
                waiter
                    .model
                    .as_ref()
                    .is_some_and(|model| self.state(model) == Some(State::Loaded))
            })
            .map(|(key, _)| *key)
            .collect();
        for key in ready {
            let ask = self
                .waiters
                .get(&key)
                .and_then(|waiter| waiter.lease.as_ref());
            if ask.is_some_and(|ask| !self.turn_free(ask)) {
                continue;
            }
            if let Some(waiter) = self.waiters.remove(&key) {
                self.admit(now, waiter, out);
            }
        }
        // A held model's room is its lease's, so no interactive waiter could have used it.
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
        let Some(model) = self.waiters.get(&key)?.model.clone() else {
            return self.serve_bare(now, key, out);
        };
        let slot = self.slots.get(&model)?;
        // Granted leases keep their turns while their model reloads, so a reload serves no waiter.
        if let Some(reason) = self.turn_reason(now, key) {
            return Some(reason);
        }
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
        // A waiter ahead may have been refused since `reason` was read, moving this one up.
        let reason = match reason {
            Reason::Turn { .. } => self.turn_reason(now, key).unwrap_or(reason),
            reason => reason,
        };
        let Some(waiter) = self.waiters.get(&key) else {
            return;
        };
        // A load serves only the turns free, so a lease past them is not served by it.
        let estimate = self
            .estimate(waiter.model.as_ref(), &reason)
            .filter(|_| matches!(reason, Reason::Turn { .. }) || !self.past_free_turns(key));
        if let Some(refusal) = waiter.refusal(now, &reason, estimate) {
            out.push(Action::Refuse {
                waiter: waiter.id,
                refusal,
            });
            if let Some(waiter) = self.waiters.remove(&key) {
                self.unserved(now, &waiter);
            }
        } else if let Some(waiter) = self.waiters.get_mut(&key) {
            out.extend(waiter.tell(reason, estimate));
        }
    }

    /// Fails every waiter `leaving` gives an error for, and takes it out of the queue
    pub(super) fn fail_waiters(
        &mut self,
        now: Moment,
        leaving: impl Fn(&Waiter) -> Option<String>,
        out: &mut Vec<Action>,
    ) {
        let failed: Vec<_> = self
            .waiters
            .iter()
            .filter_map(|(key, waiter)| leaving(waiter).map(|error| (*key, error)))
            .collect();
        for (key, error) in failed {
            if let Some(waiter) = self.waiters.remove(&key) {
                out.push(Action::Fail {
                    waiter: waiter.id,
                    error,
                });
                self.unserved(now, &waiter);
            }
        }
    }

    /// Forgets a waiter whose client went away
    pub(super) fn gone(&mut self, now: Moment, id: WaiterId) {
        let key = self
            .waiters
            .iter()
            .find(|(_, waiter)| waiter.id == id)
            .map(|(key, _)| *key);
        if let Some(waiter) = key.and_then(|key| self.waiters.remove(&key)) {
            self.unserved(now, &waiter);
        }
    }

    /// When a waiter on `wanted` held up by `reason` should be served, `wanted` being `None` for a bare lease
    fn estimate(&self, wanted: Option<&ModelName>, reason: &Reason) -> Option<Moment> {
        match reason {
            Reason::Loading { model } => self.loaded_by(model),
            Reason::Behind { model } => model.model().and_then(|model| self.loaded_by(model)),
            Reason::Held { until, .. } => *until,
            Reason::Turn {
                model,
                holders,
                ahead: 0,
            } => self.turn_freed(model, holders),
            Reason::Turn { .. } => None,
            Reason::Grace { until, .. } => {
                Some(until.plus(wanted.map_or(Duration::ZERO, |wanted| self.load_time(wanted))))
            }
            Reason::Evicting { .. } | Reason::Draining { .. } => None,
        }
    }

    /// When enough of `holders` should have ended for a turn on `model` to be free
    ///
    /// More may hold turns than `sequences` allows, after a reload lowered it
    /// or a restart brought back leases granted under a larger one. A holder
    /// with no expected end never counts as ending.
    fn turn_freed(&self, model: &ModelName, holders: &[TurnHolder]) -> Option<Moment> {
        let limit = self.config.models.get(model)?.sequences?;
        let limit = usize::try_from(limit.get()).ok()?;
        let mut ends: Vec<_> = holders.iter().map(|holder| holder.until).collect();
        ends.sort_by_key(|until| (until.is_none(), *until));
        let must_end = (holders.len() + 1).checked_sub(limit)?;
        ends.get(must_end.checked_sub(1)?).copied().flatten()
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
