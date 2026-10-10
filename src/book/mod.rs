//! The book: each model's state, who waits for it, and what to do next
//!
//! [`Book::handle`] takes one [`Event`] and returns the [`Action`]s it calls
//! for. It does no I/O and reads no clock, so every decision follows from the
//! events and the moments they carry.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    config::{Backend, ClientName, Config, ModelName, PlacementName},
    footprint::Footprint,
};

mod admit;
mod backend;
mod bare;
mod events;
mod idle;
mod lease;
mod place;
mod reload;
mod revoke;
mod snapshot;
mod turn;
mod view;
mod wait;

#[cfg(test)]
mod tests;

pub(crate) use events::{Action, Event};
use lease::Lease;
pub(crate) use lease::{Ended, Hold, LeaseAsk, LeaseId, Leased, Revocation};
pub(crate) use reload::{Found, RestoredLease};
use revoke::Revoked;
pub(crate) use snapshot::{LoadError, Snapshot, WaiterKind};
#[cfg(test)]
pub(crate) use snapshot::{ModelView, WaiterView};
pub(crate) use view::LeaseView;
use wait::Waiter;
pub(crate) use wait::{Reason, Refusal, Taker, TurnHolder};

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
// wire format: state.json holds it, so changing this is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// One model's place in the book
#[derive(Debug)]
struct Slot {
    state: State,
    /// The figures it loaded with, or its config's while Unloaded.
    footprint: Footprint,
    /// The placement it claimed room in or loaded in, until it unloads.
    placement: Option<PlacementName>,
    last_used: Moment,
    load_started: Moment,
    load_took: Option<Duration>,
    /// Its one retry is used: the next failure is final. Cleared when a load
    /// succeeds or fails again, or nothing wants the model.
    failed_once: bool,
    /// What the room goes to, while it is evicted for a waiter.
    for_model: Option<Taker>,
    /// Found loaded with no config entry and no lease.
    unknown: bool,
    /// Loaded by something other than the dog.
    stray: bool,
    /// The backend it last started loading on, or a stray or stand-in was found on.
    loaded_on: Option<Backend>,
}

impl Slot {
    fn new(footprint: Footprint) -> Slot {
        Slot {
            state: State::Unloaded,
            footprint,
            placement: None,
            last_used: Moment(0),
            load_started: Moment(0),
            load_took: None,
            failed_once: false,
            for_model: None,
            unknown: false,
            stray: false,
            loaded_on: None,
        }
    }
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
    /// Bare leases waiting on an eviction committed for them: the room the models leaving
    /// free is theirs.
    claims: BTreeSet<LeaseId>,
    /// Revoked bare leases whose job may still run: listed, and counted while their holder is
    /// attached.
    revoked: BTreeMap<LeaseId, Revoked>,
    /// Requests forwarded and not finished, by who sent them: each model's
    /// count, and whether a lease's holder has one on its model.
    in_flight_by: BTreeMap<(ClientName, ModelName), u32>,
    /// When the grace periods blocking a held model's reload end.
    reload_grace: Vec<Moment>,
    errors: VecDeque<LoadError>,
}

impl Book {
    /// A book with every configured model unloaded and nobody waiting
    pub fn new(config: Arc<Config>) -> Book {
        let slots = config
            .models
            .values()
            .map(|model| (model.name.clone(), Slot::new(model.footprint)))
            .collect();
        Book {
            config,
            slots,
            waiters: BTreeMap::new(),
            arrivals: 0,
            leases: BTreeMap::new(),
            claims: BTreeSet::new(),
            revoked: BTreeMap::new(),
            in_flight_by: BTreeMap::new(),
            reload_grace: Vec::new(),
            errors: VecDeque::new(),
        }
    }

    /// Applies `event` and returns what to do, unloads first and answers last
    pub fn handle(&mut self, now: Moment, event: Event) -> Vec<Action> {
        let mut out = Vec::new();
        // A lease past its end is gone before a late renew or attach can reach it.
        self.expire(now, &mut out);
        match event {
            Event::RequestArrived {
                waiter,
                client,
                model,
                priority,
                max_wait,
            } => {
                self.touch(now, &client, &model);
                let waiter = Waiter::request(now, waiter, client, model, max_wait);
                self.arrive(now, priority, waiter, &mut out);
            }
            Event::LeaseAsked { waiter, ask } => {
                let priority = ask.priority;
                self.arrive(now, priority, Waiter::lease(now, waiter, ask), &mut out);
            }
            Event::LeaseRenewed { lease } => self.renew(now, lease),
            Event::LeaseNoted { lease, note } => self.note(now, lease, note, &mut out),
            Event::LeaseReleased { lease } => self.end(lease, Ended::Released, &mut out),
            Event::LeaseRevoked { lease, by, note } => {
                self.revoke(lease, Revocation { by, note }, &mut out);
            }
            Event::HolderDetached { lease } => self.detach(now, lease),
            Event::HolderAttached { lease } => self.attach(lease),
            Event::WaiterGone { waiter } => self.gone(now, waiter),
            Event::RequestFinished { model, client } => self.finish(now, &client, &model, &mut out),
            Event::Loaded { model } => self.loaded(now, &model),
            Event::LoadFailed { model, error } => self.load_failed(now, &model, error, &mut out),
            Event::Unloaded { model } => self.unloaded(&model),
            Event::BackendExited { model } => self.exited(now, &model, &mut out),
            Event::StrayFound {
                model,
                footprint,
                backend,
            } => self.found_stray(now, model, footprint, backend),
            Event::Tick => {}
        }
        self.settle(now, out)
    }

    /// Serves what the change allows and returns it all, unloads first and answers last
    fn settle(&mut self, now: Moment, mut out: Vec<Action>) -> Vec<Action> {
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
        let leases = self.leases.values().flat_map(|lease| {
            [
                lease.ends_at(self.config.reconnect),
                self.idle_ends(lease).map(|(at, _)| at),
            ]
        });
        let idle = self.slots.keys().map(|model| self.idle_at(model));
        let reloads = self.reload_grace.iter().copied().map(Some);
        waiters
            .chain(leases)
            .chain(idle)
            .chain(reloads)
            .chain(self.revoked_ends().map(Some))
            .flatten()
            .min()
    }

    /// The model's state, or `None` for a model the book does not know
    pub fn state(&self, model: &ModelName) -> Option<State> {
        self.slots.get(model).map(|slot| slot.state)
    }

    /// Serves or queues a waiter, which may name only a model in the config, or a footprint the
    /// host can hold
    fn arrive(&mut self, now: Moment, priority: Priority, waiter: Waiter, out: &mut Vec<Action>) {
        let bare = waiter.lease.as_ref().and_then(LeaseAsk::bare);
        let unknown = waiter
            .model
            .as_ref()
            .filter(|model| !self.config.models.contains_key(*model));
        if let Some(model) = unknown {
            out.push(Action::Fail {
                waiter: waiter.id,
                error: format!("no model named {model}"),
            });
        } else if bare.is_some_and(|footprint| !self.config.host.ever_fits(&footprint)) {
            out.push(Action::Fail {
                waiter: waiter.id,
                error: "the footprint cannot fit the host even when alone".to_owned(),
            });
        } else if waiter
            .model
            .as_ref()
            .is_some_and(|model| self.state(model) == Some(State::Loaded))
            // A lease that takes a turn queues, so the walk serves turns in order.
            && waiter
                .lease
                .as_ref()
                .is_none_or(|ask| self.turn_limit(ask).is_none())
        {
            self.admit(now, waiter, out);
        } else {
            self.arrivals += 1;
            self.waiters.insert((priority, self.arrivals), waiter);
        }
    }

    /// Forwards a request, or grants a lease, on its Loaded model
    fn admit(&mut self, now: Moment, waiter: Waiter, out: &mut Vec<Action>) {
        if let Some(ask) = waiter.lease {
            self.grant(now, waiter.id, ask, out);
            return;
        }
        let Some(model) = waiter.model else {
            return;
        };
        if let Some(slot) = self.slots.get_mut(&model) {
            slot.last_used = now;
        }
        self.start_use(&waiter.client, &model);
        out.push(Action::Forward {
            waiter: waiter.id,
            model,
            client: waiter.client,
        });
    }
}
