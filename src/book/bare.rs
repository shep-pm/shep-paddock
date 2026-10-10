//! Bare leases: memory a job of the holder's own uses, admitted as a load is and never evicted.

use std::collections::BTreeSet;

use super::{
    Action, Book, LeaseAsk, LeaseId, Moment, Priority, Reason, Slot, State, Taker,
    admit::{Guard, Span},
};
use crate::footprint::Footprint;

impl Book {
    /// What bare leases hold in `span`, leaving out the waiter at `skip`
    ///
    /// A granted bare lease holds its memory now and later. A waiting one whose
    /// eviction is committed claims it later.
    pub(super) fn bare_figures(&self, span: Span, skip: Option<(Priority, u64)>) -> Vec<Footprint> {
        let granted = self.leases.values().filter_map(|lease| lease.ask.bare());
        let claimed = self
            .waiters
            .iter()
            .filter(|(key, _)| span == Span::Later && Some(**key) != skip)
            .filter_map(|(_, waiter)| waiter.lease.as_ref())
            .filter(|ask| self.claims.contains(&ask.lease))
            .filter_map(LeaseAsk::bare);
        granted.chain(claimed).collect()
    }

    /// Grants the bare lease waiting at `key` once its memory fits, or makes room for it
    ///
    /// It must fit beside what is held now and what is held or claimed later,
    /// as a load must. Without room it evicts as a load would, at its own
    /// priority, and claims the room until the models leave. Returns why it
    /// still waits, or `None` once it is granted.
    pub(super) fn serve_bare(
        &mut self,
        now: Moment,
        key: (Priority, u64),
        out: &mut Vec<Action>,
    ) -> Option<Reason> {
        let ask = self.waiters.get(&key)?.lease.as_ref()?;
        let wanted = ask.bare()?;
        let taker = Taker::of(ask);
        let lease = ask.lease;
        let fits = |span| self.fits(None, wanted, &[], span, Some(key));
        if fits(Span::Now) && fits(Span::Later) {
            self.claims.remove(&lease);
            self.unclaim(&taker);
            let waiter = self.waiters.remove(&key)?;
            self.admit(now, waiter, out);
            return None;
        }
        if self.claims.contains(&lease)
            && let Some(reason) = self.leaving_for(&taker)
        {
            return Some(reason);
        }
        let order = self.in_the_way(now, None, key.0);
        let free: Vec<_> = order
            .iter()
            .filter(|(guard, _)| *guard == Guard::Free)
            .map(|(_, name)| name.clone())
            .collect();
        if let Some(set) = self.eviction_set(None, wanted, free, Some(key)) {
            self.evict(set, &taker, out);
            let reason = self.leaving_for(&taker);
            return Some(reason.unwrap_or(Reason::Behind { model: taker }));
        }
        Some(self.blocked(now, &taker, &[(None, wanted)], &order, Some(key)))
    }

    /// Why a bare lease whose eviction is committed still waits: a model leaving for it, else
    /// any model leaving
    fn leaving_for(&self, taker: &Taker) -> Option<Reason> {
        let leaving = |slot: &Slot| matches!(slot.state, State::Evicting | State::Unloading);
        let (model, slot) = self
            .slots
            .iter()
            .find(|(_, slot)| leaving(slot) && slot.for_model.as_ref() == Some(taker))
            .or_else(|| self.slots.iter().find(|(_, slot)| leaving(slot)))?;
        Some(match &slot.for_model {
            Some(for_model) => Reason::Evicting {
                model: model.clone(),
                for_model: for_model.clone(),
            },
            None => Reason::Draining {
                model: model.clone(),
            },
        })
    }

    /// The reason naming the bare lease in the way that ends last, else a bare lease's claim
    pub(super) fn bare_reason(&self, now: Moment, skip: Option<(Priority, u64)>) -> Option<Reason> {
        if let Some(held) = self.last_to_end(now, |lease| lease.ask.bare().is_some()) {
            return Some(held);
        }
        self.waiters
            .iter()
            .filter(|(key, _)| Some(**key) != skip)
            .filter_map(|(_, waiter)| waiter.lease.as_ref())
            .find(|ask| self.claims.contains(&ask.lease))
            .map(|ask| Reason::Behind {
                model: Taker::of(ask),
            })
    }

    /// Drops the claim of every bare lease no longer waiting, and stops evictions naming it
    ///
    /// Evictions committed for a dropped claim stand. Returns whether any claim was dropped.
    pub(super) fn drop_bare_claims(&mut self) -> bool {
        let waiting: BTreeSet<LeaseId> = self
            .waiters
            .values()
            .filter_map(|waiter| waiter.lease.as_ref())
            .map(|ask| ask.lease)
            .collect();
        let before = self.claims.len();
        self.claims.retain(|lease| waiting.contains(lease));
        let claims = &self.claims;
        for slot in self.slots.values_mut() {
            if matches!(&slot.for_model, Some(Taker::Bare { lease, .. }) if !claims.contains(lease))
            {
                slot.for_model = None;
            }
        }
        self.claims.len() != before
    }
}
