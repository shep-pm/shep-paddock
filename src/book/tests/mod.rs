use super::{
    lease::{Hold, LeaseView},
    *,
};
use crate::{config::ClientName, test_support};

mod admit;
mod invariants;
mod lease;
mod load;
mod wait;

pub(super) fn book() -> Book {
    Book::new(test_support::config(test_support::HOST_AND_MODELS))
}

pub(super) fn book_from(toml: &str) -> Book {
    Book::new(test_support::config(toml))
}

pub(super) fn m(name: &str) -> ModelName {
    ModelName::from(name)
}

pub(super) fn ask(
    book: &mut Book,
    now: u64,
    waiter: u64,
    model: &str,
    priority: Priority,
) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::RequestArrived {
            waiter: WaiterId(waiter),
            model: m(model),
            priority,
            max_wait: Duration::from_secs(120),
        },
    )
}

/// What bench-01 asks for by default: a connection-held batch lease.
pub(super) fn lease_ask(lease: u64, model: &str) -> LeaseAsk {
    LeaseAsk {
        lease: LeaseId(lease),
        client: ClientName::from("bench-01"),
        model: m(model),
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: None,
    }
}

pub(super) fn ask_lease(book: &mut Book, now: u64, waiter: u64, ask: LeaseAsk) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::LeaseAsked {
            waiter: WaiterId(waiter),
            ask,
        },
    )
}

pub(super) fn take(
    book: &mut Book,
    now: u64,
    waiter: u64,
    lease: u64,
    model: &str,
    expected: Option<u64>,
) -> Vec<Action> {
    let ask = LeaseAsk {
        expected: expected.map(Duration::from_secs),
        ..lease_ask(lease, model)
    };
    ask_lease(book, now, waiter, ask)
}

/// Loads `model` from nothing, so a test can start from a warm host.
pub(super) fn warm(book: &mut Book, now: u64, model: &str) {
    let _ = ask(book, now, 9_000 + now, model, Priority::Interactive);
    let _ = book.handle(Moment(now), Event::Loaded { model: m(model) });
    let _ = book.handle(Moment(now), Event::RequestFinished { model: m(model) });
}

pub(super) fn forward(waiter: u64, model: &str) -> Action {
    Action::Forward {
        waiter: WaiterId(waiter),
        model: m(model),
    }
}

pub(super) fn waiting(waiter: u64, reason: Reason) -> Action {
    Action::Waiting {
        waiter: WaiterId(waiter),
        reason,
        estimate: None,
    }
}

pub(super) fn waiting_until(waiter: u64, reason: Reason, at: u64) -> Action {
    Action::Waiting {
        waiter: WaiterId(waiter),
        reason,
        estimate: Some(Moment(at)),
    }
}

pub(super) fn grant(waiter: u64, lease: u64) -> Action {
    Action::Grant {
        waiter: WaiterId(waiter),
        lease: LeaseId(lease),
    }
}

pub(super) fn ended(lease: u64, why: Ended) -> Action {
    Action::LeaseEnded {
        lease: LeaseId(lease),
        why,
    }
}

pub(super) fn tick(book: &mut Book, now: u64) -> Vec<Action> {
    book.handle(Moment(now), Event::Tick)
}

pub(super) fn loading(model: &str) -> Reason {
    Reason::Loading { model: m(model) }
}

pub(super) fn behind(model: &str) -> Reason {
    Reason::Behind { model: m(model) }
}

/// What breaks the book's promises about memory, or `None`
///
/// Derived from the slots on its own, not through the book's fit code, so
/// a fault in that code cannot hide here.
pub(super) fn broken(book: &Book) -> Option<String> {
    let now = |state| {
        matches!(
            state,
            State::Loading | State::Loaded | State::Evicting | State::Unloading
        )
    };
    let later = |state| matches!(state, State::Reserved | State::Loading | State::Loaded);
    for (set, holds) in [("now", &now as &dyn Fn(State) -> bool), ("later", &later)] {
        let held: Vec<_> = book.slots.iter().filter(|(_, s)| holds(s.state)).collect();
        if !book
            .config
            .host
            .fits(held.iter().map(|(_, s)| &s.footprint))
        {
            let names: Vec<_> = held.iter().map(|(name, _)| name.as_str()).collect();
            return Some(format!("held {set} passes the host: {names:?}"));
        }
        for (a, _) in &held {
            for (b, _) in &held {
                if a < b && book.config.excluded(a, b) {
                    return Some(format!("{a} and {b} are held together {set}"));
                }
            }
        }
    }
    let fits_beside = |model: &ModelName, holds: &dyn Fn(State) -> bool| {
        let others: Vec<_> = book
            .slots
            .iter()
            .filter(|(name, slot)| *name != model && holds(slot.state))
            .collect();
        let wanted = &book.slots[model].footprint;
        !others
            .iter()
            .any(|(name, _)| book.config.excluded(model, name))
            && book.config.host.fits(
                core::iter::once(wanted).chain(others.iter().map(|(_, slot)| &slot.footprint)),
            )
    };
    let leaving = book
        .slots
        .values()
        .any(|slot| matches!(slot.state, State::Evicting | State::Unloading));
    let stranded = book.slots.iter().find(|(name, slot)| {
        slot.state == State::Reserved && fits_beside(name, &now) && fits_beside(name, &later)
    });
    match stranded {
        Some((name, _)) if !leaving => Some(format!("{name} is reserved beside free room")),
        _ => None,
    }
}
