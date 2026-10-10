//! Turns: how many leases a model's backend serves at once, and who waits for one.

use super::{Book, LeaseAsk, Moment, Priority, Reason, TurnHolder, lease::Lease};
use crate::config::ModelName;

impl Book {
    /// The leases taking a turn on `model`: every one not reclaimable
    fn turn_takers<'a>(&'a self, model: &'a ModelName) -> impl Iterator<Item = &'a Lease> {
        self.leases
            .values()
            .filter(move |lease| !lease.ask.reclaimable && lease.ask.model() == Some(model))
    }

    /// How many turns `ask` may share, or `None` when it takes none
    ///
    /// A reclaimable or bare ask takes no turn, and a model with no `sequences` has no limit.
    pub(super) fn turn_limit(&self, ask: &LeaseAsk) -> Option<usize> {
        let limit = self.config.models.get(ask.model()?)?.sequences?;
        let limit = usize::try_from(limit.get()).unwrap_or(usize::MAX);
        (!ask.reclaimable).then_some(limit)
    }

    /// Whether `ask` may be granted on its Loaded model now, as far as turns go
    pub(super) fn turn_free(&self, ask: &LeaseAsk) -> bool {
        match (self.turn_limit(ask), ask.model()) {
            (Some(limit), Some(model)) => self.turn_takers(model).count() < limit,
            _ => true,
        }
    }

    /// How many leases wait for a turn on `ask`'s model ahead of the waiter at `key`
    fn turns_ahead(&self, key: (Priority, u64), ask: &LeaseAsk) -> usize {
        self.waiters
            .range(..key)
            .filter_map(|(_, waiter)| waiter.lease.as_ref())
            .filter(|queued| {
                queued.model().is_some() && queued.model() == ask.model() && !queued.reclaimable
            })
            .count()
    }

    /// Whether the lease waiter at `key` needs a turn its model's next load cannot give it
    pub(super) fn past_free_turns(&self, key: (Priority, u64)) -> bool {
        let Some(ask) = self
            .waiters
            .get(&key)
            .and_then(|waiter| waiter.lease.as_ref())
        else {
            return false;
        };
        self.turn_limit(ask)
            .zip(ask.model())
            .is_some_and(|(limit, model)| {
                self.turn_takers(model).count() + self.turns_ahead(key, ask) >= limit
            })
    }

    /// Why the lease waiter at `key` waits for a turn, or `None` if it needs none or one is free
    pub(super) fn turn_reason(&self, now: Moment, key: (Priority, u64)) -> Option<Reason> {
        let ask = self.waiters.get(&key)?.lease.as_ref()?;
        if self.turn_free(ask) {
            return None;
        }
        let model = ask.model()?;
        let holders = self
            .turn_takers(model)
            .map(|lease| TurnHolder {
                client: lease.ask.client.clone(),
                note: lease.ask.note.clone(),
                until: lease.until().filter(|at| *at > now),
            })
            .collect();
        Some(Reason::Turn {
            model: model.clone(),
            holders,
            ahead: self.turns_ahead(key, ask),
        })
    }
}
