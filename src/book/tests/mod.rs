use super::*;
use crate::test_support;

mod admit;
mod load;

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
        },
    )
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

pub(super) fn loading(model: &str) -> Reason {
    Reason::Loading { model: m(model) }
}

pub(super) fn behind(model: &str) -> Reason {
    Reason::Behind { model: m(model) }
}
