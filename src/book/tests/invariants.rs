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

#[derive(Debug, Clone)]
enum Op {
    Ask(usize, bool),
    Finish(usize),
    Loaded(usize),
    LoadFailed(usize),
    Unloaded(usize),
    Exited(usize),
    Gone(u64),
    Tick,
}

fn op() -> impl Strategy<Value = Op> {
    let model = 0..MODELS.len();
    prop_oneof![
        6 => (model.clone(), any::<bool>()).prop_map(|(i, batch)| Op::Ask(i, batch)),
        2 => model.clone().prop_map(Op::Finish),
        4 => model.clone().prop_map(Op::Loaded),
        1 => model.clone().prop_map(Op::LoadFailed),
        3 => model.clone().prop_map(Op::Unloaded),
        2 => model.prop_map(Op::Exited),
        1 => (0_u64..120).prop_map(Op::Gone),
        1 => Just(Op::Tick),
    ]
}

/// The event `op` stands for, or `None` when no honest engine could send it
///
/// Backends answer only what the book asked of them, so `op`'s index picks
/// among the models in the state its event needs.
fn event(book: &Book, op: &Op, waiter: u64) -> Option<Event> {
    let pick = |i: usize, wanted: &dyn Fn(&Slot) -> bool| {
        let found: Vec<_> = book.slots.iter().filter(|(_, slot)| wanted(slot)).collect();
        (!found.is_empty()).then(|| found[i % found.len()].0.clone())
    };
    let in_state = |state: State| move |slot: &Slot| slot.state == state;
    let model = match *op {
        Op::Ask(i, batch) => {
            return Some(Event::RequestArrived {
                waiter: WaiterId(waiter),
                model: m(MODELS[i]),
                priority: if batch {
                    Priority::Batch
                } else {
                    Priority::Interactive
                },
            });
        }
        Op::Gone(id) => {
            return Some(Event::WaiterGone {
                waiter: WaiterId(id),
            });
        }
        Op::Tick => return Some(Event::Tick),
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

proptest! {
    #[test]
    fn memory_held_never_passes_the_host(ops in vec(op(), 20..200)) {
        let mut book = book_from(CROWDED);
        for (at, op) in (0_u64..).zip(&ops) {
            let Some(event) = event(&book, op, at) else {
                continue;
            };
            let _ = book.handle(Moment(at), event);
            prop_assert_eq!(broken(&book), None, "after {:?} at step {}", op, at);
        }
    }
}
