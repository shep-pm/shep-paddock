use super::*;

// Small enough that random asks collide often: r excludes w, big
// takes the whole card, and p and q are sheep a reload swaps.
pub(super) const CROWDED: &str = r#"
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
sequences = 1

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

[models.q]
backend = { sheep = "q" }
url = "http://127.0.0.1:9001"
vram = "2G"
ram = "1G"
idle = "1h"
"#;

const MODELS: [&str; 7] = ["a", "y", "r", "w", "big", "p", "q"];

/// CROWDED without a, with y grown to two turns, and with p and q on each other's sheep, for reloads
/// to switch between.
pub(super) fn reloaded() -> String {
    CROWDED
        .replace("[models.a]\nbackend = \"ollama\"\nname = \"a\"\nvram = \"4G\"\nram = \"1G\"\nidle = \"1h\"\n", "")
        .replace("name = \"y\"\nvram = \"10G\"\nram = \"1G\"", "name = \"y\"\nvram = \"12G\"\nram = \"2G\"")
        .replace("idle = \"1h\"\nsequences = 1", "idle = \"1h\"\nsequences = 2")
        .replace("[models.p]\nbackend = { sheep = \"p\" }", "[models.p]\nbackend = { sheep = \"q\" }")
        .replace("[models.q]\nbackend = { sheep = \"q\" }", "[models.q]\nbackend = { sheep = \"p\" }")
}

#[derive(Debug, Clone)]
pub(super) enum Op {
    Ask(usize, bool, bool),
    Lease(
        usize,
        bool,
        bool,
        Option<u64>,
        Option<u64>,
        bool,
        Option<u64>,
    ),
    Note(usize),
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

pub(super) fn op() -> impl Strategy<Value = Op> {
    let model = 0..MODELS.len();
    let lease = 0_usize..8;
    // Steps short of, across, and far past the ttl, reconnect and grace.
    let step = prop_oneof![0_u64..5_000, 55_000_u64..130_000, 3_600_000_u64..3_700_000];
    prop_oneof![
        6 => (model.clone(), any::<bool>(), any::<bool>())
            .prop_map(|(i, batch, bench)| Op::Ask(i, batch, bench)),
        3 => (
            model.clone(),
            any::<bool>(),
            any::<bool>(),
            proptest::option::of(0_u64..300),
            proptest::option::of(0_u64..7_200),
            any::<bool>(),
            // Zero is refused before it reaches the book, and would end a lease at its grant.
            proptest::option::of(1_u64..600),
        )
            .prop_map(
                |(i, batch, heartbeat, max_wait, expected, reclaimable, idle)| {
                    Op::Lease(i, batch, heartbeat, max_wait, expected, reclaimable, idle)
                }
            ),
        2 => model.clone().prop_map(Op::Finish),
        4 => model.clone().prop_map(Op::Loaded),
        1 => model.clone().prop_map(Op::LoadFailed),
        3 => model.clone().prop_map(Op::Unloaded),
        2 => model.prop_map(Op::Exited),
        1 => (0_u64..120).prop_map(Op::Gone),
        1 => lease.clone().prop_map(Op::Renew),
        1 => lease.clone().prop_map(Op::Note),
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
/// among the models in the state its event needs, among the requests in
/// flight, or among granted leases.
pub(super) fn event(book: &Book, op: &Op, waiter: u64) -> Option<Event> {
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
        Op::Ask(i, batch, bench) => {
            let client = if bench { "bench-01" } else { "mac-sessions" };
            return Some(Event::RequestArrived {
                waiter: WaiterId(waiter),
                client: ClientName::from(client),
                model: m(MODELS[i]),
                priority: priority(batch),
                max_wait: Duration::from_secs(120),
            });
        }
        Op::Lease(i, batch, heartbeat, max_wait, expected, reclaimable, idle) => {
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
                reclaimable,
                release_if_idle: idle.map(Duration::from_secs),
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
        Op::Note(i) => {
            return lease(i).map(|lease| Event::LeaseNoted {
                lease,
                note: "step".to_owned(),
            });
        }
        Op::Finish(i) => {
            let pairs: Vec<_> = book.in_flight_by.keys().collect();
            return (!pairs.is_empty()).then(|| {
                let (client, model) = pairs[i % pairs.len()].clone();
                Event::RequestFinished { model, client }
            });
        }
        Op::Release(i) => return lease(i).map(|lease| Event::LeaseReleased { lease }),
        Op::Detach(i) => return lease(i).map(|lease| Event::HolderDetached { lease }),
        Op::Attach(i) => return lease(i).map(|lease| Event::HolderAttached { lease }),
        Op::Tick(_) => return Some(Event::Tick),
        Op::Reconfigure => return None,
        Op::Loaded(i) | Op::LoadFailed(i) => pick(i, &in_state(State::Loading))?,
        Op::Unloaded(i) => pick(i, &in_state(State::Unloading))?,
        Op::Exited(i) => pick(i, &|slot| slot.state != State::Unloaded)?,
    };
    Some(match op {
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
