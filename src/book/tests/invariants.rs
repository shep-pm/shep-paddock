use std::collections::BTreeSet;

use proptest::{collection::vec, prelude::*};

use super::*;
use crate::config::PlacementName;

mod ops;

use ops::{CROWDED, Op, event, op, reloaded};

/// Asked leases by id, each with its model and whether it is reclaimable;
/// granted held leases with their model and whether its backend has exited
/// since the grant or its last reload; and granted reclaimable leases. Kept
/// from outside the book.
#[derive(Debug, Default)]
struct Granted {
    asked: BTreeMap<LeaseId, (ModelName, bool)>,
    live: BTreeMap<LeaseId, (ModelName, bool)>,
    reclaimable: BTreeSet<LeaseId>,
}

impl Granted {
    fn saw_event(&mut self, event: &Event) {
        match event {
            Event::LeaseAsked { ask, .. } => {
                self.asked
                    .insert(ask.lease, (ask.model.clone(), ask.reclaimable));
            }
            Event::BackendExited { model } => {
                for (held, exited) in self.live.values_mut() {
                    *exited |= held == model;
                }
            }
            _ => {}
        }
    }

    fn saw_actions(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::Grant { lease, .. } => match self.asked.get(lease) {
                    Some((_, true)) => {
                        self.reclaimable.insert(*lease);
                    }
                    Some((model, false)) => {
                        self.live.insert(*lease, (model.clone(), false));
                    }
                    None => {}
                },
                Action::LeaseEnded { lease, .. } => {
                    self.live.remove(lease);
                    self.reclaimable.remove(lease);
                }
                _ => {}
            }
        }
    }

    /// A held model that is not Loaded with no exit to explain it
    ///
    /// A held model is never evicted. It may be anything but Evicting while
    /// it loads again after an exit, and the exit is forgotten once it has.
    fn broken(&mut self, book: &Book) -> Option<String> {
        let found = self.live.iter().find_map(|(lease, (model, exited))| {
            let state = book.state(model);
            let excused = *exited && state != Some(State::Evicting);
            (state != Some(State::Loaded) && !excused)
                .then(|| format!("{model} is {state:?} under lease {lease:?}"))
        });
        for (model, exited) in self.live.values_mut() {
            *exited &= book.state(model) != Some(State::Loaded);
        }
        found
    }

    /// A waiter told, or refused, because of a reclaimable lease
    fn blocked_by_reclaimable(&self, actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|action| {
            let reason = match action {
                Action::Waiting { reason, .. } => reason,
                Action::Refuse { refusal, .. } => &refusal.reason,
                _ => return None,
            };
            match reason {
                Reason::Held { lease, .. } if self.is_reclaimable(lease) => {
                    Some(format!("{action:?} names reclaimable lease {lease:?}"))
                }
                Reason::Behind { model } if self.only_reclaimable(model) => Some(format!(
                    "{action:?} waits behind {model}, which only reclaimable leases name"
                )),
                _ => None,
            }
        })
    }

    /// Whether `lease` was asked as reclaimable, so one granted this step counts too
    fn is_reclaimable(&self, lease: &LeaseId) -> bool {
        self.asked
            .get(lease)
            .is_some_and(|(_, reclaimable)| *reclaimable)
    }

    /// Whether live reclaimable leases name `model` and no held one does
    fn only_reclaimable(&self, model: &ModelName) -> bool {
        let named = |lease: &LeaseId| {
            self.asked
                .get(lease)
                .is_some_and(|(asked, _)| asked == model)
        };
        self.reclaimable.iter().any(named) && !self.live.values().any(|(held, _)| held == model)
    }
}

/// A model that started loading, or claimed room, past the host or beside an exclusion
///
/// Each model counts at the larger of the figures it loaded with and its
/// config's, which is what the gate promises. A load or claim must fit in
/// every set it joins: now and later for a load, later for a claim. A held
/// model unloading after a crash is in the later set, since it loads again.
///
/// It repeats the shape of the book's own fit code on purpose, so a fault there
/// cannot hide here. Beside `Config::excluded`, two models touching one sheep
/// are excluded. A model holding memory touches the sheep in `ran_on`, kept
/// from outside the book. A load in `actions` joins, so a retry counts too.
/// Ollama stand-ins and moved ollama models are left to the unit tests.
fn admitted_over(
    book: &Book,
    before: &BTreeMap<ModelName, State>,
    ran_on: &BTreeMap<ModelName, String>,
    actions: &[Action],
) -> Option<String> {
    let counted = |name: &ModelName, slot: &Slot| {
        let Some(configured) = book.config.models.get(name) else {
            return slot.footprint;
        };
        let declared =
            |placement: &PlacementName| configured.placements.iter().any(|p| p.name == *placement);
        match &slot.placement {
            Some(placement) if !declared(placement) => slot.footprint,
            placement => slot
                .footprint
                .larger(configured.footprint_at(placement.as_ref())),
        }
    };
    let now = |_: &ModelName, slot: &Slot| {
        matches!(
            slot.state,
            State::Loading | State::Loaded | State::Evicting | State::Unloading
        )
    };
    let sheep = |name: &ModelName, slot: &Slot| {
        let configured = book.config.models.get(name).and_then(|m| m.backend.sheep());
        let running = ran_on.get(name).filter(|_| now(name, slot));
        [configured, running.map(String::as_str)]
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>()
    };
    let leases = book.leases();
    let later = |name: &ModelName, slot: &Slot| match slot.state {
        State::Reserved | State::Loading | State::Loaded => true,
        State::Unloading => leases.iter().any(|lease| lease.model == *name),
        _ => false,
    };
    let fits_beside = |model: &ModelName, holds: &dyn Fn(&ModelName, &Slot) -> bool| {
        let others: Vec<_> = book
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && holds(name, slot))
            .collect();
        let touched = sheep(model, &book.slots[model]);
        let excluded = others.iter().any(|(name, slot)| {
            book.config.excluded(model, name) || !touched.is_disjoint(&sheep(name, slot))
        });
        let figures: Vec<_> = core::iter::once(counted(model, &book.slots[model]))
            .chain(others.iter().map(|(name, slot)| counted(name, slot)))
            .collect();
        !excluded && book.config.host.fits(&figures)
    };
    book.slots.iter().find_map(|(name, slot)| {
        let loads = actions.contains(&Action::Load(name.clone()));
        let joined = loads || before.get(name) != Some(&slot.state);
        let over = match slot.state {
            State::Loading => !fits_beside(name, &now) || !fits_beside(name, &later),
            State::Reserved => !fits_beside(name, &later),
            _ => false,
        };
        (joined && over).then(|| format!("{name} went {:?} past the host", slot.state))
    })
}

/// A live reclaimable lease whose model is not Loaded
///
/// A grant needs its model Loaded, a restore ends one whose model was not
/// found loaded, and every way a model leaves Loaded ends its reclaimable
/// leases first. So one never outlives its model. `admitted_over` counts an
/// Unloading model any lease names as claiming room, which is sound only
/// while this holds.
fn outlived(book: &Book) -> Option<String> {
    book.leases()
        .into_iter()
        .filter(|lease| lease.reclaimable)
        .find_map(|lease| {
            let state = book.state(&lease.model);
            (state != Some(State::Loaded)).then(|| {
                format!(
                    "reclaimable lease {:?} names {} while it is {state:?}",
                    lease.id, lease.model
                )
            })
        })
}

/// A lease ended idle although its holder was using it when the step began
fn idle_in_use(in_use: &BTreeSet<LeaseId>, actions: &[Action]) -> Option<String> {
    actions.iter().find_map(|action| match action {
        Action::LeaseEnded {
            lease,
            why: Ended::Idle { .. },
        } if in_use.contains(lease) => Some(format!("lease {lease:?} ended idle while in use")),
        _ => None,
    })
}

/// A model that held memory before and after a step but changed placement
///
/// A load that failed, a backend that exited, or an unload that finished ends what was
/// running. So the model the step's event named may start loading again elsewhere within
/// the step, but a model that keeps holding memory keeps its placement.
fn moved(
    book: &Book,
    before: &BTreeMap<ModelName, (State, Option<PlacementName>)>,
    named: Option<&ModelName>,
) -> Option<String> {
    let running = |state: State| {
        matches!(
            state,
            State::Loading | State::Loaded | State::Evicting | State::Unloading
        )
    };
    book.slots.iter().find_map(|(name, slot)| {
        let (was, placed) = before.get(name)?;
        let restarted = Some(name) == named && slot.state == State::Loading;
        let ran_on = running(*was) && running(slot.state) && !restarted;
        (ran_on && *placed != slot.placement)
            .then(|| format!("{name} moved from {placed:?} to {:?}", slot.placement))
    })
}

/// A grant past a model's `sequences`, a lease waiting for a turn on a Loaded model with one
/// free, or a turn granted while a lease queued ahead of it on that model still waits
///
/// Counted from the config and the leases, not through the book's turn code.
fn turns(
    book: &Book,
    queued: &BTreeMap<LeaseId, (Priority, u64)>,
    actions: &[Action],
) -> Option<String> {
    let limit = |ask: &LeaseAsk| {
        let limit = book.config.models.get(&ask.model)?.sequences?;
        (!ask.reclaimable).then(|| usize::try_from(limit.get()).unwrap_or(usize::MAX))
    };
    let taken = |model: &ModelName| {
        book.leases
            .values()
            .filter(|lease| !lease.ask.reclaimable && lease.ask.model == *model)
            .count()
    };
    let granted: Vec<&LeaseAsk> = actions
        .iter()
        .filter_map(|action| match action {
            Action::Grant { lease, .. } => book.leases.get(lease).map(|held| &held.ask),
            _ => None,
        })
        .filter(|ask| limit(ask).is_some())
        .collect();
    let over = granted.iter().find_map(|ask| {
        let (limit, taken) = (limit(ask)?, taken(&ask.model));
        (taken > limit).then(|| format!("{} has {taken} turns taken of {limit}", ask.model))
    });
    let stranded = || {
        book.waiters.values().find_map(|waiter| {
            let ask = waiter.lease.as_ref()?;
            let free = limit(ask)? > taken(&ask.model);
            (book.state(&ask.model) == Some(State::Loaded) && free).then(|| {
                format!(
                    "lease {:?} waits with a turn free on {}",
                    ask.lease, ask.model
                )
            })
        })
    };
    // An ask that was not queued before the event arrived in it, behind every one queued.
    let jumped = || {
        granted.iter().find_map(|ask| {
            let before = |key: &(Priority, u64)| match queued.get(&ask.lease) {
                Some(granted) => key < granted,
                None => key.0 <= ask.priority,
            };
            book.waiters.iter().find_map(|(key, waiter)| {
                let waiting = waiter.lease.as_ref()?;
                (waiting.model == ask.model && !waiting.reclaimable && before(key)).then(|| {
                    format!(
                        "lease {:?} took a turn ahead of {:?}",
                        ask.lease, waiting.lease
                    )
                })
            })
        })
    };
    over.or_else(stranded).or_else(jumped)
}

proptest! {
    #[test]
    fn memory_held_never_passes_the_host(ops in vec(op(), 20..200)) {
        let configs = [test_support::config(CROWDED), test_support::config(&reloaded())];
        assert!(!configs[1].models.contains_key(&m("a")));
        let y = |config: &Config| config.models[&m("y")].footprint;
        assert_ne!(y(&configs[0]), y(&configs[1]));
        let turns_of = |config: &Config| config.models[&m("y")].sequences;
        assert_ne!(turns_of(&configs[0]), turns_of(&configs[1]), "a reload changes y's turns");
        let p = |config: &Config| config.models[&m("p")].backend.clone();
        assert_ne!(p(&configs[0]), p(&configs[1]), "a reload moves p between sheep");
        let mut book = Book::new(configs[0].clone());
        let mut granted = Granted::default();
        let mut now = 0_u64;
        let mut reloads = 0_usize;
        // The sheep each model's last load started on, under the config of its step.
        let mut ran_on: BTreeMap<ModelName, String> = BTreeMap::new();
        for (at, op) in (0_u64..).zip(&ops) {
            now += match op {
                Op::Tick(step) => *step,
                _ => 1,
            };
            let mut before: BTreeMap<_, _> = book
                .slots
                .iter()
                .map(|(name, slot)| (name.clone(), slot.state))
                .collect();
            let placed_before: BTreeMap<_, _> = book
                .slots
                .iter()
                .map(|(name, slot)| (name.clone(), (slot.state, slot.placement.clone())))
                .collect();
            let in_use: BTreeSet<_> = book
                .leases()
                .into_iter()
                .filter(|lease| lease.in_use)
                .map(|lease| lease.id)
                .collect();
            let queued: BTreeMap<_, _> = book
                .waiters
                .iter()
                .filter_map(|(key, waiter)| waiter.lease.as_ref().map(|ask| (ask.lease, *key)))
                .collect();
            let mut named = None;
            let actions = if let Op::Reconfigure = op {
                // A reload makes every Reserved model claim its room again.
                before.values_mut().for_each(|state| {
                    if *state == State::Reserved {
                        *state = State::Unloaded;
                    }
                });
                reloads += 1;
                book.reconfigure(Moment(now), configs[reloads % 2].clone())
            } else {
                let Some(event) = event(&book, op, at) else {
                    continue;
                };
                granted.saw_event(&event);
                named = match &event {
                    Event::LoadFailed { model, .. }
                    | Event::BackendExited { model }
                    | Event::Unloaded { model } => Some(model.clone()),
                    _ => None,
                };
                book.handle(Moment(now), event)
            };
            prop_assert_eq!(
                granted.blocked_by_reclaimable(&actions),
                None,
                "after {:?} at step {}", op, at
            );
            prop_assert_eq!(idle_in_use(&in_use, &actions), None, "after {:?} at step {}", op, at);
            granted.saw_actions(&actions);
            for action in &actions {
                if let Action::Load(model) = action {
                    match book.config.models.get(model).and_then(|c| c.backend.sheep()) {
                        Some(sheep) => ran_on.insert(model.clone(), sheep.to_owned()),
                        None => ran_on.remove(model),
                    };
                }
            }
            prop_assert_eq!(broken(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(
                admitted_over(&book, &before, &ran_on, &actions),
                None,
                "after {:?} at step {}", op, at
            );
            prop_assert_eq!(granted.broken(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(outlived(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(turns(&book, &queued, &actions), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(
                moved(&book, &placed_before, named.as_ref()),
                None,
                "after {:?} at step {}", op, at
            );
            prop_assert!(
                book.next_deadline().is_none_or(|deadline| deadline > Moment(now)),
                "a deadline at or before now after {:?} at step {}", op, at
            );
        }
    }
}
