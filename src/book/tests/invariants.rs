use proptest::{collection::vec, prelude::*};

use super::*;
use crate::config::PlacementName;

// Small enough that random asks collide often: r excludes w, and big
// takes the whole card.
const CROWDED: &str = r#"
[host]
vram = "24G"
ram = "16G"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.a]
backend = "ollama"
name = "a"
vram = "4G"
ram = "1G"
idle = "1h"

[models.y]
backend = "ollama"
name = "y"
vram = "10G"
ram = "1G"
idle = "1h"

[models.r]
backend = "ollama"
name = "r"
vram = "14G"
ram = "1G"
excludes = ["w"]
idle = "1h"

[models.w]
backend = "ollama"
name = "w"
ram = "8G"
idle = "1h"

[models.big]
backend = "ollama"
name = "big"
vram = "all"
ram = "4G"
idle = "1h"

[models.p]
backend = { sheep = "p" }
url = "http://127.0.0.1:9000"
idle = "1h"

[[models.p.placements]]
name = "gpu"
vram = "8G"
ram = "1G"
env = { DEVICE = "cuda" }

[[models.p.placements]]
name = "ram"
ram = "6G"
env = { DEVICE = "cpu" }
"#;

const MODELS: [&str; 6] = ["a", "y", "r", "w", "big", "p"];

/// CROWDED without a, and with y grown, for reloads to switch between.
fn reloaded() -> String {
    CROWDED
        .replace("[models.a]\nbackend = \"ollama\"\nname = \"a\"\nvram = \"4G\"\nram = \"1G\"\nidle = \"1h\"\n", "")
        .replace("name = \"y\"\nvram = \"10G\"\nram = \"1G\"", "name = \"y\"\nvram = \"12G\"\nram = \"2G\"")
}

#[derive(Debug, Clone)]
enum Op {
    Ask(usize, bool),
    Lease(usize, bool, bool, Option<u64>, Option<u64>),
    Finish(usize),
    Loaded(usize),
    LoadFailed(usize),
    Unloaded(usize),
    Exited(usize),
    Gone(u64),
    Renew(usize),
    Release(usize),
    Detach(usize),
    Attach(usize),
    Tick(u64),
    Reconfigure,
}

fn op() -> impl Strategy<Value = Op> {
    let model = 0..MODELS.len();
    let lease = 0_usize..8;
    // Steps short of, across, and far past the ttl, reconnect and grace.
    let step = prop_oneof![0_u64..5_000, 55_000_u64..130_000, 3_600_000_u64..3_700_000];
    prop_oneof![
        6 => (model.clone(), any::<bool>()).prop_map(|(i, batch)| Op::Ask(i, batch)),
        3 => (
            model.clone(),
            any::<bool>(),
            any::<bool>(),
            proptest::option::of(0_u64..300),
            proptest::option::of(0_u64..7_200),
        )
            .prop_map(|(i, batch, heartbeat, max_wait, expected)| {
                Op::Lease(i, batch, heartbeat, max_wait, expected)
            }),
        2 => model.clone().prop_map(Op::Finish),
        4 => model.clone().prop_map(Op::Loaded),
        1 => model.clone().prop_map(Op::LoadFailed),
        3 => model.clone().prop_map(Op::Unloaded),
        2 => model.prop_map(Op::Exited),
        1 => (0_u64..120).prop_map(Op::Gone),
        1 => lease.clone().prop_map(Op::Renew),
        1 => lease.clone().prop_map(Op::Release),
        1 => lease.clone().prop_map(Op::Detach),
        1 => lease.prop_map(Op::Attach),
        2 => step.prop_map(Op::Tick),
        1 => Just(Op::Reconfigure),
    ]
}

/// The event `op` stands for, or `None` when no honest engine could send it
///
/// Backends answer only what the book asked of them, so `op`'s index picks
/// among the models in the state its event needs, or among granted leases.
fn event(book: &Book, op: &Op, waiter: u64) -> Option<Event> {
    let pick = |i: usize, wanted: &dyn Fn(&Slot) -> bool| {
        let found: Vec<_> = book.slots.iter().filter(|(_, slot)| wanted(slot)).collect();
        (!found.is_empty()).then(|| found[i % found.len()].0.clone())
    };
    let in_state = |state: State| move |slot: &Slot| slot.state == state;
    let lease = |i: usize| {
        let leases = book.leases();
        (!leases.is_empty()).then(|| leases[i % leases.len()].id)
    };
    let model = match *op {
        Op::Ask(i, batch) => {
            return Some(Event::RequestArrived {
                waiter: WaiterId(waiter),
                client: ClientName::from("mac-sessions"),
                model: m(MODELS[i]),
                priority: priority(batch),
                max_wait: Duration::from_secs(120),
            });
        }
        Op::Lease(i, batch, heartbeat, max_wait, expected) => {
            let hold = if heartbeat {
                Hold::Heartbeat {
                    ttl: Duration::from_secs(60),
                }
            } else {
                Hold::Connection
            };
            let ask = LeaseAsk {
                priority: priority(batch),
                hold,
                max_wait: max_wait.map(Duration::from_secs),
                expected: expected.map(Duration::from_secs),
                ..lease_ask(waiter, MODELS[i])
            };
            return Some(Event::LeaseAsked {
                waiter: WaiterId(waiter),
                ask,
            });
        }
        Op::Gone(id) => {
            return Some(Event::WaiterGone {
                waiter: WaiterId(id),
            });
        }
        Op::Renew(i) => return lease(i).map(|lease| Event::LeaseRenewed { lease }),
        Op::Release(i) => return lease(i).map(|lease| Event::LeaseReleased { lease }),
        Op::Detach(i) => return lease(i).map(|lease| Event::HolderDetached { lease }),
        Op::Attach(i) => return lease(i).map(|lease| Event::HolderAttached { lease }),
        Op::Tick(_) => return Some(Event::Tick),
        Op::Reconfigure => return None,
        Op::Finish(i) => pick(i, &|slot| slot.in_flight > 0)?,
        Op::Loaded(i) | Op::LoadFailed(i) => pick(i, &in_state(State::Loading))?,
        Op::Unloaded(i) => pick(i, &in_state(State::Unloading))?,
        Op::Exited(i) => pick(i, &|slot| slot.state != State::Unloaded)?,
    };
    Some(match op {
        Op::Finish(_) => Event::RequestFinished { model },
        Op::Loaded(_) => Event::Loaded { model },
        Op::LoadFailed(_) => Event::LoadFailed {
            model,
            error: "failed".to_owned(),
        },
        Op::Unloaded(_) => Event::Unloaded { model },
        _ => Event::BackendExited { model },
    })
}

fn priority(batch: bool) -> Priority {
    if batch {
        Priority::Batch
    } else {
        Priority::Interactive
    }
}

/// Granted leases by id, each with its model and whether its backend has
/// exited since the grant or its last reload, kept from outside the book.
#[derive(Debug, Default)]
struct Granted {
    asked: BTreeMap<LeaseId, ModelName>,
    live: BTreeMap<LeaseId, (ModelName, bool)>,
}

impl Granted {
    fn saw_event(&mut self, event: &Event) {
        match event {
            Event::LeaseAsked { ask, .. } => {
                self.asked.insert(ask.lease, ask.model.clone());
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
                Action::Grant { lease, .. } => {
                    if let Some(model) = self.asked.get(lease) {
                        self.live.insert(*lease, (model.clone(), false));
                    }
                }
                Action::LeaseEnded { lease, .. } => {
                    self.live.remove(lease);
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
}

/// A model that started loading, or claimed room, past the host or beside an exclusion
///
/// Each model counts at the larger of the figures it loaded with and its
/// config's, which is what the gate promises. A load or claim must fit in
/// every set it joins: now and later for a load, later for a claim. A held
/// model unloading after a crash is in the later set, since it loads again.
///
/// It repeats the shape of the book's own fit code on purpose, so a fault there
/// cannot hide here. It asks `Config::excluded`, not `Book::excluded`, so
/// exclusions from ollama stand-ins are covered by the unit tests only: the
/// generated books have none.
fn admitted_over(book: &Book, before: &BTreeMap<ModelName, State>) -> Option<String> {
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
        let excluded = others
            .iter()
            .any(|(name, _)| book.config.excluded(model, name));
        let figures: Vec<_> = core::iter::once(counted(model, &book.slots[model]))
            .chain(others.iter().map(|(name, slot)| counted(name, slot)))
            .collect();
        !excluded && book.config.host.fits(&figures)
    };
    book.slots.iter().find_map(|(name, slot)| {
        let joined = before.get(name) != Some(&slot.state);
        let over = match slot.state {
            State::Loading => !fits_beside(name, &now) || !fits_beside(name, &later),
            State::Reserved => !fits_beside(name, &later),
            _ => false,
        };
        (joined && over).then(|| format!("{name} went {:?} past the host", slot.state))
    })
}

/// A model that held memory before and after a step but changed placement
///
/// A load that failed, a backend that exited, or an unload that finished ends what was
/// running, so the model the step's event named may start again elsewhere within the step.
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
        let ran_on = running(*was) && running(slot.state) && Some(name) != named;
        (ran_on && *placed != slot.placement)
            .then(|| format!("{name} moved from {placed:?} to {:?}", slot.placement))
    })
}

proptest! {
    #[test]
    fn memory_held_never_passes_the_host(ops in vec(op(), 20..200)) {
        let configs = [test_support::config(CROWDED), test_support::config(&reloaded())];
        assert!(!configs[1].models.contains_key(&m("a")));
        let y = |config: &Config| config.models[&m("y")].footprint;
        assert_ne!(y(&configs[0]), y(&configs[1]));
        let mut book = Book::new(configs[0].clone());
        let mut granted = Granted::default();
        let mut now = 0_u64;
        let mut reloads = 0_usize;
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
            granted.saw_actions(&actions);
            prop_assert_eq!(broken(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(admitted_over(&book, &before), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(granted.broken(&book), None, "after {:?} at step {}", op, at);
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
