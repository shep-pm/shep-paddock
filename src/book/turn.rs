//! Turns: how many conversations a model's backend serves at once, and who waits for one.
//!
//! A lease that is not reclaimable takes a turn, and one past `sequences` waits for one. A turn
//! has one request in flight. Requests under no lease's turn share one extra request turn.

use super::{
    Action, Book, LeaseAsk, LeaseId, Moment, Priority, Reason, State, TurnHolder, TurnKind, Waiter,
    WaiterId, lease::Lease,
};
use crate::config::{ClientName, ModelName};

/// One conversation of a model's backend, which has one request in flight at a time
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Turn {
    /// The turn a lease takes.
    Lease(LeaseId),
    /// The model's one extra turn, for requests that use no lease's turn.
    Request(ModelName),
}

/// A request in flight on a turn
#[derive(Debug)]
pub(super) struct Taken {
    pub turn: Turn,
    pub client: ClientName,
    pub model: ModelName,
}

/// The lease as the holder of its turn
fn holder(now: Moment, lease: &Lease) -> TurnHolder {
    TurnHolder {
        client: lease.ask.client.clone(),
        note: lease.ask.note.clone(),
        until: lease.until().filter(|at| *at > now),
    }
}

impl Book {
    /// The requests in flight on a lease turn of `model` whose lease has since ended
    ///
    /// They take their turn with them only when they finish, so it counts as held until then.
    fn orphans<'a>(&'a self, model: &'a ModelName) -> impl Iterator<Item = &'a Taken> {
        self.turns.values().filter(move |taken| {
            taken.model == *model
                && matches!(taken.turn, Turn::Lease(lease) if !self.leases.contains_key(&lease))
        })
    }

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
    ///
    /// The request turn is not counted, but a lease turn whose lease ended with its request still
    /// in flight is, until that request finishes.
    pub(super) fn turn_free(&self, ask: &LeaseAsk) -> bool {
        match (self.turn_limit(ask), ask.model()) {
            (Some(limit), Some(model)) => {
                self.turn_takers(model).count() + self.orphans(model).count() < limit
            }
            _ => true,
        }
    }

    /// Whether a request for `model` queues for a turn: `model` has `sequences`
    pub(super) fn has_turns(&self, model: &ModelName) -> bool {
        self.config
            .models
            .get(model)
            .is_some_and(|model| model.sequences.is_some())
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

    /// Who holds a turn of `model` a lease takes, for a waiter to be told
    fn lease_holders(&self, now: Moment, model: &ModelName) -> Vec<TurnHolder> {
        let leases = self.turn_takers(model).map(|lease| holder(now, lease));
        let orphans = self.orphans(model).map(|taken| TurnHolder {
            client: taken.client.clone(),
            note: None,
            until: None,
        });
        leases.chain(orphans).collect()
    }

    /// Why the waiter at `key` waits for a turn, or `None` if it needs none or one is free
    ///
    /// A lease waits for a turn even while its model is reloading, since the leases granted keep
    /// theirs. A request waits for one only once its model is Loaded.
    pub(super) fn turn_reason(&self, now: Moment, key: (Priority, u64)) -> Option<Reason> {
        let waiter = self.waiters.get(&key)?;
        let Some(ask) = waiter.lease.as_ref() else {
            let loaded = waiter
                .model
                .as_ref()
                .is_some_and(|model| self.state(model) == Some(State::Loaded));
            return loaded
                .then(|| self.draw(now, key).err().map(|reason| *reason))
                .flatten();
        };
        if self.turn_free(ask) {
            return None;
        }
        let model = ask.model()?;
        Some(Reason::Turn {
            model: model.clone(),
            turn: TurnKind::Lease,
            holders: self.lease_holders(now, model),
            ahead: self.turns_ahead(key, ask),
        })
    }

    /// The turn the request waiter at `key` takes now, or why it waits
    ///
    /// `Ok(None)` is a request that needs none, because its model has no `sequences`. It is
    /// resolved afresh at every call, from the client's leases as they are.
    ///
    /// A request that names one of its client's leases that takes a turn on the model takes that
    /// lease's turn, and waits for it and no other when it is busy. A lease that has ended, is
    /// another client's, is on another model, or takes no turn is passed over, whenever this is
    /// called, so a request waiting on a lease that ends falls through to the rules below.
    ///
    /// A client with leases that take a turn takes the first of them that is free, each earlier
    /// request of its own for the model going first, whatever the priority, and those naming a
    /// lease taking that one. When all are busy it waits for the first to free. A client with none takes the request turn, which the walk
    /// hands out interactive first, then by arrival.
    pub(super) fn draw(
        &self,
        now: Moment,
        key: (Priority, u64),
    ) -> Result<Option<Turn>, Box<Reason>> {
        let Some(waiter) = self.waiters.get(&key).filter(|w| w.lease.is_none()) else {
            return Ok(None);
        };
        let Some(model) = waiter.model.as_ref().filter(|m| self.has_turns(m)) else {
            return Ok(None);
        };
        let own: Vec<LeaseId> = self
            .turn_takers(model)
            .filter(|lease| lease.ask.client == waiter.client)
            .map(|lease| lease.ask.lease)
            .collect();
        let requests =
            |queued: &Waiter| queued.lease.is_none() && queued.model.as_ref() == Some(model);
        if own.is_empty() {
            let turn = Turn::Request(model.clone());
            let Some(holder) = self.turns.values().find(|taken| taken.turn == turn) else {
                return Ok(Some(turn));
            };
            let ahead = self
                .waiters
                .range(..key)
                .filter(|(_, queued)| requests(queued) && !self.takes_lease_turn(queued))
                .count();
            return Err(Box::new(Reason::Turn {
                model: model.clone(),
                turn: TurnKind::Request,
                holders: vec![TurnHolder {
                    client: holder.client.clone(),
                    note: None,
                    until: None,
                }],
                ahead,
            }));
        }
        let busy = |lease: &LeaseId| {
            self.turns
                .values()
                .any(|taken| taken.turn == Turn::Lease(*lease))
        };
        // A lease the request names counts when it is one of the client's that takes a turn on
        // the model, and the request then waits for that turn and no other.
        if let Some(lease) = waiter.named.filter(|lease| own.contains(lease)) {
            if !busy(&lease) {
                return Ok(Some(Turn::Lease(lease)));
            }
            let ahead = self
                .waiters
                .iter()
                .filter(|(queued_key, queued)| {
                    queued_key.1 < key.1
                        && requests(queued)
                        && queued.client == waiter.client
                        && queued.named == Some(lease)
                })
                .count();
            return Err(Box::new(Reason::Turn {
                model: model.clone(),
                turn: TurnKind::Lease,
                holders: self.busy_holders(now, &[lease]),
                ahead,
            }));
        }
        // Requests of its own that came first take the free turns first, except those that name
        // a lease, which take that one's turn.
        let names = |queued: &Waiter| queued.named.filter(|lease| own.contains(lease));
        let earlier = |(queued_key, queued): &(&(Priority, u64), &Waiter)| {
            queued_key.1 < key.1 && requests(queued) && queued.client == waiter.client
        };
        let ahead = self
            .waiters
            .iter()
            .filter(earlier)
            .filter(|(_, queued)| names(queued).is_none())
            .count();
        let named: Vec<LeaseId> = self
            .waiters
            .iter()
            .filter(earlier)
            .filter_map(|(_, queued)| names(queued))
            .collect();
        let mut free = own
            .iter()
            .copied()
            .filter(|lease| !busy(lease) && !named.contains(lease));
        match free.nth(ahead) {
            Some(lease) => Ok(Some(Turn::Lease(lease))),
            None => Err(Box::new(Reason::Turn {
                model: model.clone(),
                turn: TurnKind::Lease,
                holders: self.busy_holders(now, &own),
                ahead,
            })),
        }
    }

    /// Whether the request `waiter` is under a lease that takes a turn
    fn takes_lease_turn(&self, waiter: &Waiter) -> bool {
        waiter.model.as_ref().is_some_and(|model| {
            self.turn_takers(model)
                .any(|lease| lease.ask.client == waiter.client)
        })
    }

    /// Records the turn a request holds from `waiter`'s forwarding until it finishes
    pub(super) fn take_turn(
        &mut self,
        waiter: WaiterId,
        client: &ClientName,
        model: &ModelName,
        turn: Turn,
    ) {
        self.turns.insert(
            waiter,
            Taken {
                turn,
                client: client.clone(),
                model: model.clone(),
            },
        );
    }

    /// Who holds the turns of `leases` that have a request in flight, for a request waiting on them
    fn busy_holders(&self, now: Moment, leases: &[LeaseId]) -> Vec<TurnHolder> {
        leases
            .iter()
            .filter(|lease| {
                self.turns
                    .values()
                    .any(|taken| taken.turn == Turn::Lease(**lease))
            })
            .filter_map(|lease| self.leases.get(lease))
            .map(|lease| holder(now, lease))
            .collect()
    }

    /// Admits the waiter at `key`, which `turn_reason` found nothing to hold back
    pub(super) fn admit_waiting(
        &mut self,
        now: Moment,
        key: (Priority, u64),
        out: &mut Vec<Action>,
    ) {
        let turn = self.draw(now, key).ok().flatten();
        if let Some(waiter) = self.waiters.remove(&key) {
            self.admit(now, waiter, turn, out);
        }
    }
}
