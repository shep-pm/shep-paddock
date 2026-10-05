use proptest::{collection::vec, prelude::*};

use super::*;

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
"#;

const MODELS: [&str; 5] = ["a", "y", "r", "w", "big"];

/// CROWDED without a, and with y grown, for reloads to switch between.
fn reloaded() -> String {
    CROWDED
        .replace("[models.a]\nbackend = \"ollama\"\nname = \"a\"\nvram = \"4G\"\nram = \"1G\"\nidle = \"1h\"\n", "")
        .replace("name = \"y\"\nvram = \"10G\"\nram = \"1G\"", "name = \"y\"\nvram = \"12G\"\nram = \"2G\"")
}

#[derive(Debug, Clone)]
enum Op {
    Ask(usize, bool),
    Lease(usize, bool, bool),
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
        3 => (model.clone(), any::<bool>(), any::<bool>())
            .prop_map(|(i, batch, heartbeat)| Op::Lease(i, batch, heartbeat)),
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
        Op::Lease(i, batch, heartbeat) => {
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

proptest! {
    #[test]
    fn memory_held_never_passes_the_host(ops in vec(op(), 20..200)) {
        let configs = [test_support::config(CROWDED), test_support::config(&reloaded())];
        assert!(!configs[1].models.contains_key(&m("a")));
        let mut book = Book::new(configs[0].clone());
        let mut granted = Granted::default();
        let mut now = 0_u64;
        let mut reloads = 0_usize;
        for (at, op) in (0_u64..).zip(&ops) {
            now += match op {
                Op::Tick(step) => *step,
                _ => 1,
            };
            let actions = if let Op::Reconfigure = op {
                reloads += 1;
                book.reconfigure(Moment(now), configs[reloads % 2].clone())
            } else {
                let Some(event) = event(&book, op, at) else {
                    continue;
                };
                granted.saw_event(&event);
                book.handle(Moment(now), event)
            };
            granted.saw_actions(&actions);
            prop_assert_eq!(broken(&book), None, "after {:?} at step {}", op, at);
            prop_assert_eq!(granted.broken(&book), None, "after {:?} at step {}", op, at);
        }
    }
}
