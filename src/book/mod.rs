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
mod wait;

#[cfg(test)]
mod tests;

pub(crate) use wait::Reason;
use wait::Waiter;

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
}

/// One waiting request, as the engine names it
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
    /// Has room claimed, and loads once the models evicted for it unload.
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
    },
    /// A waiting request's client went away.
    WaiterGone {
        /// The request.
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
    /// Answer the request with an error.
    Fail {
        /// The request.
        waiter: WaiterId,
        /// What went wrong.
        error: String,
    },
    /// Tell the request why it still waits.
    Waiting {
        /// The request.
        waiter: WaiterId,
        /// Why it waits.
        reason: Reason,
        /// When it should be served, when that can be said.
        estimate: Option<Moment>,
    },
}

impl Action {
    /// Unloads free room before loads take it, and answers come last
    fn rank(&self) -> u8 {
        match self {
            Self::Unload(_) => 0,
            Self::Load(_) => 1,
            Self::Forward { .. } => 2,
            Self::Fail { .. } => 3,
            Self::Waiting { .. } => 4,
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
            } => self.arrive(now, waiter, model, priority, &mut out),
            Event::WaiterGone { waiter } => self.waiters.retain(|_, w| w.id != waiter),
            Event::RequestFinished { model } => self.finish(now, &model, &mut out),
            Event::Loaded { model } => self.loaded(now, &model),
            Event::LoadFailed { model, error } => self.load_failed(now, &model, error, &mut out),
            Event::Unloaded { model } => self.unloaded(now, &model, &mut out),
            Event::BackendExited { model } => self.exited(now, &model, &mut out),
            Event::Tick => {}
        }
        self.reconsider(now, &mut out);
        out.sort_by_key(Action::rank);
        out
    }

    /// The model's state, or `None` for a model the book does not know
    pub fn state(&self, model: &ModelName) -> Option<State> {
        self.slots.get(model).map(|slot| slot.state)
    }

    fn arrive(
        &mut self,
        now: Moment,
        waiter: WaiterId,
        model: ModelName,
        priority: Priority,
        out: &mut Vec<Action>,
    ) {
        match self.state(&model) {
            None => out.push(Action::Fail {
                waiter,
                error: format!("no model named {model}"),
            }),
            Some(State::Loaded) => self.admit(now, waiter, model, out),
            Some(_) => {
                self.arrivals += 1;
                let key = (priority, self.arrivals);
                self.waiters.insert(key, Waiter::new(waiter, model));
            }
        }
    }

    fn admit(&mut self, now: Moment, waiter: WaiterId, model: ModelName, out: &mut Vec<Action>) {
        if let Some(slot) = self.slots.get_mut(&model) {
            slot.in_flight = slot.in_flight.saturating_add(1);
            slot.last_used = now;
        }
        out.push(Action::Forward { waiter, model });
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

    fn unloaded(&mut self, now: Moment, model: &ModelName, out: &mut Vec<Action>) {
        let Some(slot) = self.slots.get_mut(model) else {
            return;
        };
        slot.state = State::Unloaded;
        let Some(reserved) = slot.for_model.take() else {
            return;
        };
        let pending = self
            .slots
            .values()
            .any(|slot| slot.for_model.as_ref() == Some(&reserved));
        if !pending && self.state(&reserved) == Some(State::Reserved) {
            self.start_load(now, &reserved, out);
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

    /// Serves every waiter that can be served, in order
    ///
    /// Waiters on a Loaded model are admitted before anything is evicted,
    /// so a model is never evicted from under a waiter ready to use it.
    fn reconsider(&mut self, now: Moment, out: &mut Vec<Action>) {
        let ready: Vec<_> = self
            .waiters
            .iter()
            .filter(|(_, waiter)| self.state(&waiter.model) == Some(State::Loaded))
            .map(|(key, _)| *key)
            .collect();
        for key in ready {
            self.serve(now, key, out);
        }
        let keys: Vec<_> = self.waiters.keys().copied().collect();
        for key in keys {
            self.serve(now, key, out);
        }
    }

    fn serve(&mut self, now: Moment, key: (Priority, u64), out: &mut Vec<Action>) {
        let Some(model) = self.waiters.get(&key).map(|waiter| waiter.model.clone()) else {
            return;
        };
        let Some(slot) = self.slots.get(&model) else {
            return;
        };
        let reason = match slot.state {
            State::Loaded => {
                if let Some(waiter) = self.waiters.remove(&key) {
                    self.admit(now, waiter.id, model, out);
                }
                return;
            }
            State::Reserved | State::Loading => Reason::Loading { model },
            State::Evicting | State::Unloading => match slot.for_model.clone() {
                Some(for_model) => Reason::Evicting { model, for_model },
                None => Reason::Draining { model },
            },
            State::Unloaded => self.make_room(now, model, out),
        };
        if let Some(waiter) = self.waiters.get_mut(&key) {
            out.extend(waiter.tell(reason));
        }
    }

    /// Loads `model`, or evicts for it, or names what it waits behind
    fn make_room(&mut self, now: Moment, model: ModelName, out: &mut Vec<Action>) -> Reason {
        if self.fits(&model, &[]) {
            self.start_load(now, &model, out);
            return Reason::Loading { model };
        }
        match self.eviction_set(&model, self.candidates(&model)) {
            Some(set) => {
                self.evict(set, &model, out);
                Reason::Loading { model }
            }
            None => Reason::Behind {
                model: self.blocker(&model),
            },
        }
    }
}
