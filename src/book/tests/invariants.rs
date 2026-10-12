use std::collections::BTreeSet;

use proptest::{collection::vec, prelude::*};

use super::*;
use crate::config::PlacementName;

mod granted;
mod ops;

use granted::Granted;
use ops::{CROWDED, Flying, Op, event, op, reloaded};

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
///
/// Bare leases count from the book's fields: each granted one and each revoked one still counted,
/// now and later, and each claim later. A bare grant must fit beside all of them.
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
    // Bare leases held now, and held or claimed once the models leaving are gone.
    let bare_now: Vec<Footprint> = book
        .leases
        .values()
        .filter_map(|lease| lease.ask.bare())
        .chain(
            book.revoked
                .values()
                .filter(|revoked| revoked.counted)
                .filter_map(|revoked| revoked.lease.ask.bare()),
        )
        .collect();
    let bare_later: Vec<Footprint> = bare_now
        .iter()
        .copied()
        .chain(
            book.waiters
                .values()
                .filter_map(|waiter| waiter.lease.as_ref())
                .filter(|ask| book.claims.contains(&ask.lease))
                .filter_map(LeaseAsk::bare),
        )
        .collect();
    let leases = book.leases();
    let later = |name: &ModelName, slot: &Slot| match slot.state {
        State::Reserved | State::Loading | State::Loaded => true,
        State::Unloading => {
            slot.refreshing
                || leases
                    .iter()
                    .any(|lease| lease.model.as_ref() == Some(name))
        }
        _ => false,
    };
    let fits_beside =
        |model: &ModelName, holds: &dyn Fn(&ModelName, &Slot) -> bool, bare: &[Footprint]| {
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
                .chain(bare.iter().copied())
                .collect();
            !excluded && book.config.host.fits(&figures)
        };
    let over = book.slots.iter().find_map(|(name, slot)| {
        let loads = actions.contains(&Action::Load(name.clone()));
        let joined = loads || before.get(name) != Some(&slot.state);
        let over = match slot.state {
            State::Loading => {
                !fits_beside(name, &now, &bare_now) || !fits_beside(name, &later, &bare_later)
            }
            State::Reserved => !fits_beside(name, &later, &bare_later),
            _ => false,
        };
        (joined && over).then(|| format!("{name} went {:?} past the host", slot.state))
    });
    // A bare lease granted this step fits beside every model and every other bare lease.
    let bare_granted = || {
        actions.iter().find_map(|action| {
            let Action::Grant { lease, .. } = action else {
                return None;
            };
            let wanted = book.leases.get(lease)?.ask.bare()?;
            let others = |bare: &[Footprint]| {
                let mut rest = bare.to_vec();
                if let Some(at) = rest.iter().position(|held| *held == wanted) {
                    rest.remove(at);
                }
                rest
            };
            let fits = |holds: &dyn Fn(&ModelName, &Slot) -> bool, bare: &[Footprint]| {
                let models = book
                    .slots
                    .iter()
                    .filter(|(name, slot)| holds(name, slot))
                    .map(|(name, slot)| counted(name, slot));
                let figures: Vec<_> = core::iter::once(wanted)
                    .chain(models)
                    .chain(others(bare))
                    .collect();
                book.config.host.fits(&figures)
            };
            (!fits(&now, &bare_now) || !fits(&later, &bare_later))
                .then(|| format!("bare lease {lease:?} was granted past the host"))
        })
    };
    over.or_else(bare_granted)
}

/// A live reclaimable lease whose model is not Loaded
///
/// A grant needs its model Loaded, a restore ends one whose model was not
/// found loaded, and every way a model leaves Loaded ends its reclaimable
/// leases first. So one never outlives its model, except through a reload: a
/// `refreshing` model unloads and loads again with its reclaimable leases kept.
/// `admitted_over` counts an Unloading model any lease names, or that is
/// refreshing, as claiming room, which is sound only while this holds.
fn outlived(book: &Book) -> Option<String> {
    book.leases()
        .into_iter()
        .filter(|lease| lease.reclaimable)
        .find_map(|lease| {
            let model = lease.model.as_ref()?;
            let state = book.state(model);
            let refreshing = book.slots.get(model).is_some_and(|slot| slot.refreshing);
            (state != Some(State::Loaded) && !refreshing).then(|| {
                format!(
                    "reclaimable lease {:?} names {model} while it is {state:?}",
                    lease.id
                )
            })
        })
}

/// A model that began a stale reload in a step while a lease holds it, a request is in flight on
/// it, or a waiter names it
///
/// A reload unloads a running model, so it may start only when nothing would be cut off. A
/// reclaimable lease does not count, since it survives the reload. `flying` is the test's own list
/// of requests forwarded and not finished, and the waiters are read from the queue, whether a
/// request or a lease.
fn a_stale_reload_never_cuts_off_a_holder(
    book: &Book,
    refreshing_before: &BTreeSet<ModelName>,
    flying: &[Flying],
    actions: &[Action],
) -> Option<String> {
    actions.iter().find_map(|action| {
        let Action::Unload(model) = action else {
            return None;
        };
        let started = book.slots.get(model).is_some_and(|slot| slot.refreshing)
            && !refreshing_before.contains(model);
        if !started {
            return None;
        }
        let held = book
            .leases
            .values()
            .any(|lease| !lease.ask.reclaimable && lease.ask.model() == Some(model));
        let in_flight = flying.iter().any(|(_, _, on)| on == model);
        let waited = book.waiters.values().any(|waiter| {
            waiter.model.as_ref() == Some(model)
                || waiter.lease.as_ref().and_then(LeaseAsk::model) == Some(model)
        });
        (held || in_flight || waited).then(|| {
            format!("{model} reloads while held: {held}, in flight: {in_flight}, awaited: {waited}")
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
        let limit = book.config.models.get(ask.model()?)?.sequences?;
        (!ask.reclaimable).then(|| usize::try_from(limit.get()).unwrap_or(usize::MAX))
    };
    // A lease that ended with its request in flight holds its turn until the request ends.
    let taken = |model: &ModelName| {
        let leases = book
            .leases
            .values()
            .filter(|lease| !lease.ask.reclaimable && lease.ask.model() == Some(model))
            .count();
        let orphans = book
            .turns
            .values()
            .filter(|held| held.model == *model)
            .filter(|held| matches!(held.turn, Turn::Lease(id) if !book.leases.contains_key(&id)))
            .count();
        leases + orphans
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
        let model = ask.model()?;
        let (limit, taken) = (limit(ask)?, taken(model));
        (taken > limit).then(|| format!("{model} has {taken} turns taken of {limit}"))
    });
    let stranded = || {
        book.waiters.values().find_map(|waiter| {
            let ask = waiter.lease.as_ref()?;
            let model = ask.model()?;
            let free = limit(ask)? > taken(model);
            (book.state(model) == Some(State::Loaded) && free)
                .then(|| format!("lease {:?} waits with a turn free on {model}", ask.lease))
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
                let same = waiting.model().is_some() && waiting.model() == ask.model();
                (same && !waiting.reclaimable && before(key)).then(|| {
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

/// Two requests in flight on one turn, a request of a client holding a lease turn on the request
/// turn, or a batch request on the request turn while an interactive one waits for it
///
/// `leased` lists the leases that took a turn before the step, as (lease, client, model), and
/// `priorities` the priority each request arrived with. The book's record of who holds which turn
/// is read, but each rule is checked against the leases and the queue.
fn request_turns(
    book: &Book,
    leased: &[(LeaseId, ClientName, ModelName)],
    priorities: &BTreeMap<WaiterId, Priority>,
    flying: &[Flying],
    actions: &[Action],
) -> Option<String> {
    let turns: Vec<_> = book.turns.iter().collect();
    let shared = turns.iter().find_map(|(waiter, taken)| {
        turns
            .iter()
            .find(|(other, held)| other < waiter && held.turn == taken.turn)
            .map(|(other, _)| {
                format!(
                    "{other:?} and {waiter:?} are both in flight on {:?}",
                    taken.turn
                )
            })
    });
    let forgotten = turns.iter().find_map(|(waiter, _)| {
        (!flying.iter().any(|(flying, ..)| flying == *waiter))
            .then(|| format!("{waiter:?} holds a turn but is not in flight"))
    });
    let holds_lease_turn = |client: &ClientName, model: &ModelName| {
        book.leases.values().any(|lease| {
            !lease.ask.reclaimable
                && lease.ask.client == *client
                && lease.ask.model() == Some(model)
        })
    };
    let on_request_turn = |waiter: &WaiterId| {
        let taken = book.turns.get(waiter)?;
        matches!(taken.turn, Turn::Request(_)).then_some(taken)
    };
    let forwarded = actions.iter().filter_map(|action| match action {
        Action::Forward {
            waiter,
            model,
            client,
        } => on_request_turn(waiter).map(|_| (*waiter, model, client)),
        _ => None,
    });
    let stolen = forwarded.clone().find_map(|(waiter, model, client)| {
        leased
            .iter()
            .any(|(lease, holder, on)| {
                holder == client && on == model && book.leases.contains_key(lease)
            })
            .then(|| format!("{waiter:?} of {client:?} took the request turn beside its lease"))
    });
    let jumped = forwarded.clone().find_map(|(waiter, model, _)| {
        (priorities.get(&waiter) == Some(&Priority::Batch)).then_some(())?;
        book.waiters.iter().find_map(|(key, queued)| {
            let waits = queued.lease.is_none()
                && queued.model.as_ref() == Some(model)
                && key.0 == Priority::Interactive
                && !holds_lease_turn(&queued.client, model);
            waits.then(|| {
                format!(
                    "batch {waiter:?} took the request turn ahead of {:?}",
                    queued.id
                )
            })
        })
    });
    shared.or(forgotten).or(stolen).or(jumped)
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
        let mut flying: Vec<Flying> = Vec::new();
        let mut priorities: BTreeMap<WaiterId, Priority> = BTreeMap::new();
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
            let refreshing_before: BTreeSet<_> = book
                .slots
                .iter()
                .filter(|(_, slot)| slot.refreshing)
                .map(|(name, _)| name.clone())
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
            let leased: Vec<_> = book
                .leases
                .values()
                .filter(|lease| !lease.ask.reclaimable)
                .filter_map(|lease| {
                    Some((lease.ask.lease, lease.ask.client.clone(), lease.ask.model()?.clone()))
                })
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
                let Some(event) = event(&book, op, at, &flying) else {
                    continue;
                };
                granted.saw_event(&event);
                match &event {
                    Event::RequestArrived { waiter, priority, .. } => {
                        priorities.insert(*waiter, *priority);
                    }
                    Event::RequestFinished { waiter, .. } => {
                        flying.retain(|(flying, ..)| flying != waiter);
                    }
                    _ => {}
                }
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
            prop_assert_eq!(granted.bare_evicted(&actions), None, "after {:?} at step {}", op, at);
            granted.saw_actions(&actions);
            for action in &actions {
                if let Action::Forward { waiter, client, model } = action {
                    flying.push((*waiter, client.clone(), model.clone()));
                }
            }
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
            prop_assert_eq!(granted.bare_miscounted(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(outlived(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(
                a_stale_reload_never_cuts_off_a_holder(&book, &refreshing_before, &flying, &actions),
                None,
                "after {:?} at step {}", op, at
            );
            prop_assert_eq!(turns(&book, &queued, &actions), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(
                request_turns(&book, &leased, &priorities, &flying, &actions),
                None,
                "after {:?} at step {}", op, at
            );
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
