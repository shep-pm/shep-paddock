//! Whether a lease is in use: its holder's requests, its notes, and its release once idle.

use std::collections::BTreeSet;

use super::{Action, Book, Ended, Hold, LeaseId, Moment, Waiter, lease::Lease};
use crate::config::{ClientName, ModelName};

impl Book {
    /// Marks the leases `client` holds on `model` as used at `now`
    pub(super) fn touch(&mut self, now: Moment, client: &ClientName, model: &ModelName) {
        for lease in self.leases.values_mut() {
            if lease.ask.client == *client && lease.ask.model() == Some(model) {
                lease.last_activity = now;
            }
        }
    }

    /// Counts one more of `client`'s requests for `model` in flight
    pub(super) fn start_use(&mut self, client: &ClientName, model: &ModelName) {
        *self
            .in_flight_by
            .entry((client.clone(), model.clone()))
            .or_default() += 1;
    }

    /// Ends one of `client`'s requests for `model`, which is use at `now`
    pub(super) fn end_use(&mut self, now: Moment, client: &ClientName, model: &ModelName) {
        let key = (client.clone(), model.clone());
        if let Some(count) = self.in_flight_by.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight_by.remove(&key);
            }
        }
        self.touch(now, client, model);
    }

    /// How many requests for `model` are in flight, from every client
    pub(super) fn in_flight_on(&self, model: &ModelName) -> u32 {
        self.in_flight_by
            .iter()
            .filter(|((_, on), _)| on == model)
            .map(|(_, count)| *count)
            .sum()
    }

    /// A request of `waiter`'s leaving the queue unserved, which ends it, so is use at `now`
    pub(super) fn unserved(&mut self, now: Moment, waiter: &Waiter) {
        if waiter.lease.is_none()
            && let Some(model) = &waiter.model
        {
            self.touch(now, &waiter.client, model);
        }
    }

    /// Whether a request of `lease`'s holder's for its model is in flight or queued
    pub(super) fn in_use(&self, lease: &Lease) -> bool {
        let Some(leased) = lease.ask.model() else {
            return false;
        };
        let holders = |client: &ClientName, model: &ModelName| {
            *client == lease.ask.client && *model == *leased
        };
        self.in_flight_by
            .keys()
            .any(|(client, model)| holders(client, model))
            || self.waiters.values().any(|waiter| {
                waiter.lease.is_none()
                    && waiter
                        .model
                        .as_ref()
                        .is_some_and(|model| holders(&waiter.client, model))
            })
    }

    /// The granted leases whose holder has a request for its model in flight or queued
    pub fn in_use_leases(&self) -> BTreeSet<LeaseId> {
        self.leases
            .iter()
            .filter(|(_, lease)| self.in_use(lease))
            .map(|(id, _)| *id)
            .collect()
    }

    /// A progress note: use now, the lease's note from now on, and a renewal of a heartbeat lease
    pub(super) fn note(&mut self, now: Moment, id: LeaseId, note: String, out: &mut Vec<Action>) {
        let Some(lease) = self.leases.get_mut(&id) else {
            return;
        };
        lease.last_activity = now;
        lease.ask.note = Some(note);
        if matches!(lease.ask.hold, Hold::Heartbeat { .. }) {
            lease.renewed = now;
        }
        out.push(Action::Persist);
    }

    /// When `lease` ends for sitting idle, and how: never unless it asked, and not while in use
    pub(super) fn idle_ends(&self, lease: &Lease) -> Option<(Moment, Ended)> {
        let after = lease.ask.release_if_idle?;
        (!self.in_use(lease)).then(|| (lease.last_activity.plus(after), Ended::Idle { after }))
    }
}
