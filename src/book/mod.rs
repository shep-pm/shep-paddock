//! The book: each model's state, who waits for it, and what to do next
//!
//! [`Book::handle`] takes one [`Event`] and returns the [`Action`]s it calls
//! for. It does no I/O and reads no clock, so every decision follows from the
//! events and the moments they carry.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use crate::{
    config::{Config, ModelName},
    footprint::Footprint,
};

mod admit;
mod lease;
mod wait;

#[cfg(test)]
mod tests;

use lease::Lease;
pub(crate) use lease::{Ended, LeaseAsk, LeaseId};
use wait::Waiter;
pub(crate) use wait::{Reason, Refusal};

// The spec's figure for how many load failures the status keeps.
const ERRORS_KEPT: usize = 20;

/// Milliseconds since the engine started. The Book never reads a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Moment(pub u64);

impl Moment {
    /// How long after `earlier` this is, or zero if it is not after it
    pub fn since(self, earlier: Moment) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }

    /// The moment `after` this one, held at the end of time
    pub fn plus(self, after: Duration) -> Moment {
        let after = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        Moment(self.0.saturating_add(after))
    }
}

/// One waiting request or lease, as the engine names it
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WaiterId(pub u64);

/// Which waiters are served first
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Priority {
    /// Served ahead of batch waiters.
    Interactive,
    /// Served after interactive waiters.
    Batch,
}

/// Where a model is between unloaded and loaded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Holds nothing.
    Unloaded,
    /// Has room claimed, and loads once the memory it needs is free.
    Reserved,
    /// Its backend is loading it.
    Loading,
    /// Serving.
    Loaded,
    /// Evicted, and finishing the requests in flight before it unloads.
    Evicting,
    /// Its backend is unloading it.
    Unloading,
}

/// Something that happened, for the Book to decide on
#[derive(Debug)]
pub(crate) enum Event {
    /// A request for `model` arrived.
    RequestArrived {
        /// Names the request in the actions that answer it.
        waiter: WaiterId,
        /// The model asked for.
        model: ModelName,
        /// Where it queues.
        priority: Priority,
        /// How long it may wait before it is refused.
        max_wait: Duration,
    },
    /// A client asked for a lease.
    LeaseAsked {
        /// Names the lease in the actions that answer it until it is granted.
        waiter: WaiterId,
        /// What was asked for.
        ask: LeaseAsk,
    },
    /// A heartbeat lease's holder renewed it.
    LeaseRenewed {
        /// The lease.
        lease: LeaseId,
    },
    /// A lease's holder released it.
    LeaseReleased {
        /// The lease.
        lease: LeaseId,
    },
    /// A connection lease's stream broke without a release.
    HolderDetached {
        /// The lease.
        lease: LeaseId,
    },
    /// A connection lease's holder attached to it again.
    HolderAttached {
        /// The lease.
        lease: LeaseId,
    },
    /// A waiting request's or lease's client went away.
    WaiterGone {
        /// The request or lease.
        waiter: WaiterId,
    },
    /// A forwarded request's response ended.
    RequestFinished {
        /// The model that served it.
        model: ModelName,
    },
    /// A load finished and the model is ready.
    Loaded {
        /// The model.
        model: ModelName,
    },
    /// A load failed or was not ready in time.
    LoadFailed {
        /// The model.
        model: ModelName,
        /// What the backend said.
        error: String,
    },
    /// An unload finished.
    Unloaded {
        /// The model.
        model: ModelName,
    },
    /// The process serving the model exited.
    BackendExited {
        /// The model.
        model: ModelName,
    },
    /// Time passed.
    Tick,
}

/// What the engine is to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start loading the model.
    Load(ModelName),
    /// Start unloading the model.
    Unload(ModelName),
    /// Send the request on to its model.
    Forward {
        /// The request.
        waiter: WaiterId,
        /// The model to send it to.
        model: ModelName,
    },
    /// Tell the lease's holder it holds its model.
    Grant {
        /// The waiter that asked for the lease.
        waiter: WaiterId,
        /// The lease.
        lease: LeaseId,
    },
    /// Answer the waiter that it is busy, and why.
    Refuse {
        /// The request or lease.
        waiter: WaiterId,
        /// Why, and when to try again.
        refusal: Refusal,
    },
    /// Answer the waiter with an error.
    Fail {
        /// The request or lease.
        waiter: WaiterId,
        /// What went wrong.
        error: String,
    },
    /// Tell the waiter why it still waits.
    Waiting {
        /// The request or lease.
        waiter: WaiterId,
        /// Why it waits.
        reason: Reason,
        /// When it should be served, when that can be said.
        estimate: Option<Moment>,
    },
    /// Tell the lease's holder that it ended.
    LeaseEnded {
        /// The lease.
        lease: LeaseId,
        /// How it ended.
        why: Ended,
    },
    /// Save the leases, since one was granted or ended.
    Persist,
}

impl Action {
    /// Unloads free room before loads take it, and answers come last
    fn rank(&self) -> u8 {
        match self {
            Self::Unload(_) => 0,
            Self::Load(_) => 1,
            Self::Forward { .. } | Self::Grant { .. } => 2,
            Self::Fail { .. } | Self::Refuse { .. } => 3,
            Self::Waiting { .. } => 4,
            Self::LeaseEnded { .. } => 5,
            Self::Persist => 6,
        }
    }
}

/// A load that failed twice
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadError {
    /// The model.
    pub model: ModelName,
    /// When the second attempt failed.
    pub at: Moment,
    /// What the backend said.
    pub error: String,
}

/// One model's place in the book
#[derive(Debug)]
struct Slot {
    state: State,
    footprint: Footprint,
    idle: Duration,
    in_flight: u32,
    last_used: Moment,
    load_started: Moment,
    load_took: Option<Duration>,
    failed_once: bool,
    /// The Reserved model this one is being evicted for.
    for_model: Option<ModelName>,
}

/// Which models are loaded, who waits for what, and what to do next
#[derive(Debug)]
pub(crate) struct Book {
    config: Arc<Config>,
    slots: BTreeMap<ModelName, Slot>,
    /// Keyed so iteration is the order waiters are served in.
    waiters: BTreeMap<(Priority, u64), Waiter>,
    arrivals: u64,
    leases: BTreeMap<LeaseId, Lease>,
    errors: VecDeque<LoadError>,
}

impl Book {
    /// A book with every configured model unloaded and nobody waiting
    pub fn new(config: Arc<Config>) -> Book {
        let slots = config
            .models
            .values()
            .map(|model| {
                let slot = Slot {
                    state: State::Unloaded,
                    footprint: model.footprint,
                    idle: model.idle,
                    in_flight: 0,
                    last_used: Moment(0),
                    load_started: Moment(0),
                    load_took: None,
                    failed_once: false,
                    for_model: None,
                };
                (model.name.clone(), slot)
            })
            .collect();
        Book {
            config,
            slots,
            waiters: BTreeMap::new(),
            arrivals: 0,
            leases: BTreeMap::new(),
            errors: VecDeque::new(),
        }
    }

    /// Applies `event` and returns what to do, unloads first and answers last
    pub fn handle(&mut self, now: Moment, event: Event) -> Vec<Action> {
        let mut out = Vec::new();
        match event {
            Event::RequestArrived {
                waiter,
                model,
                priority,
                max_wait,
            } => {
                let waiter = Waiter::request(waiter, model, now.plus(max_wait));
                self.arrive(now, priority, waiter, &mut out);
            }
            Event::LeaseAsked { waiter, ask } => {
                let priority = ask.priority;
                self.arrive(now, priority, Waiter::lease(now, waiter, ask), &mut out);
            }
            Event::LeaseRenewed { lease } => self.renew(now, lease),
            Event::LeaseReleased { lease } => self.end(now, lease, Ended::Released, &mut out),
            Event::HolderDetached { lease } => self.detach(now, lease),
            Event::HolderAttached { lease } => self.attach(lease),
            Event::WaiterGone { waiter } => self.waiters.retain(|_, w| w.id != waiter),
            Event::RequestFinished { model } => self.finish(now, &model, &mut out),
            Event::Loaded { model } => self.loaded(now, &model),
            Event::LoadFailed { model, error } => self.load_failed(now, &model, error, &mut out),
            Event::Unloaded { model } => self.unloaded(&model),
            Event::BackendExited { model } => self.exited(now, &model, &mut out),
            Event::Tick => {}
        }
        self.expire(now, &mut out);
        self.reconsider(now, &mut out);
        self.unload_idle(now, &mut out);
        out.sort_by_key(Action::rank);
        // One save covers every grant and end this event caused.
        if out.contains(&Action::Persist) {
            out.retain(|action| *action != Action::Persist);
            out.push(Action::Persist);
        }
        out
    }

    /// The earliest moment a `Tick` may change something, if any
    pub fn next_deadline(&self) -> Option<Moment> {
        let waiters = self
            .waiters
            .values()
            .flat_map(|waiter| [waiter.deadline, waiter.grace_ends()]);
        let leases = self
            .leases
            .values()
            .map(|lease| lease.ends_at(self.config.reconnect));
        let idle = self.slots.keys().map(|model| self.idle_at(model));
        waiters.chain(leases).chain(idle).flatten().min()
    }

    /// The model's state, or `None` for a model the book does not know
    pub fn state(&self, model: &ModelName) -> Option<State> {
        self.slots.get(model).map(|slot| slot.state)
    }

    fn arrive(&mut self, now: Moment, priority: Priority, waiter: Waiter, out: &mut Vec<Action>) {
        match self.state(&waiter.model) {
            None => out.push(Action::Fail {
                waiter: waiter.id,
                error: format!("no model named {}", waiter.model),
            }),
            Some(State::Loaded) => self.admit(now, waiter, out),
            Some(_) => {
                self.arrivals += 1;
                self.waiters.insert((priority, self.arrivals), waiter);
            }
        }
    }

    /// Forwards a request, or grants a lease, on its Loaded model
    fn admit(&mut self, now: Moment, waiter: Waiter, out: &mut Vec<Action>) {
        if let Some(ask) = waiter.lease {
            self.grant(now, waiter.id, ask, out);
            return;
        }
        if let Some(slot) = self.slots.get_mut(&waiter.model) {
            slot.in_flight = slot.in_flight.saturating_add(1);
            slot.last_used = now;
        }
        out.push(Action::Forward {
            waiter: waiter.id,
            model: waiter.model,
        });
    }

    fn finish(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        slot.in_flight = slot.in_flight.saturating_sub(1);
        slot.last_used = now;
        if slot.state == State::Evicting && slot.in_flight == 0 {
            slot.state = State::Unloading;
            out.push(Action::Unload(model.clone()));
        }
    }

    fn loaded(&mut self, now: Moment, model: &ModelName) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        if slot.state == State::Loading {
            slot.state = State::Loaded;
            slot.load_took = Some(now.since(slot.load_started));
            slot.last_used = now;
        }
    }

    fn load_failed(
        &mut self,
        now: Moment,
        model: &ModelName,
        error: String,
        out: &mut Vec<Action>,
    ) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        if slot.state != State::Loading {
            return;
        }
        if !slot.failed_once {
            slot.failed_once = true;
            slot.load_started = now;
            out.push(Action::Load(model.clone()));
            return;
        }
        slot.state = State::Unloaded;
        slot.failed_once = false;
        self.waiters.retain(|_, waiter| {
            if waiter.model != *model {
                return true;
            }
            out.push(Action::Fail {
                waiter: waiter.id,
                error: error.clone(),
            });
            false
        });
        self.errors.push_back(LoadError {
            model: model.clone(),
            at: now,
            error,
        });
        if self.errors.len() > ERRORS_KEPT {
            self.errors.pop_front();
        }
    }

    fn unloaded(&mut self, model: &ModelName) {
        if let Some(slot) = self.slots.get_mut(model) {
            slot.state = State::Unloaded;
            slot.for_model = None;
        }
    }

    fn exited(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        match slot.state {
            State::Loaded | State::Evicting => {
                slot.state = State::Unloading;
                out.push(Action::Unload(model.clone()));
            }
            State::Loading => {
                let error = "backend exited while loading".to_owned();
                self.load_failed(now, model, error, out);
            }
            State::Unloaded | State::Reserved | State::Unloading => {}
        }
    }
}
