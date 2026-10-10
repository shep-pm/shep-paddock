use super::{
    lease::Hold,
    reload::{Found, RestoredLease},
    snapshot::{ModelView, WaiterKind, WaiterView},
    view::LeaseView,
    *,
};
use crate::{config::ClientName, footprint::Vram, test_support};

pub(super) const MIB: u64 = 1 << 20;
pub(super) const GIB: u64 = 1 << 30;
pub(super) const QWEN: &str = "qwen3.8:27b";

mod admit;
mod bare;
mod idle;
mod invariants;
mod lease;
mod load;
mod moved;
mod place;
mod reclaim;
mod reload;
mod restore;
mod revoke;
mod stray;
mod turn;
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
            client: ClientName::from("mac-sessions"),
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
        leased: Leased::Model(m(model)),
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: None,
        reclaimable: false,
        release_if_idle: None,
    }
}

/// bench-01's connection-held batch bare lease `lease`, declaring `vram` and `ram_gib` GiB of RAM.
pub(super) fn bare(lease: u64, vram: Vram, ram_gib: u64) -> LeaseAsk {
    LeaseAsk {
        leased: Leased::Bare {
            footprint: Footprint {
                vram,
                ram: ram_gib * GIB,
            },
            pid: None,
        },
        ..lease_ask(lease, QWEN)
    }
}

/// What a reason calls [`bare`]'s lease.
pub(super) fn bare_taker(lease: u64, vram: Vram, ram_gib: u64) -> Taker {
    Taker::Bare {
        lease: LeaseId(lease),
        client: ClientName::from("bench-01"),
        footprint: Footprint {
            vram,
            ram: ram_gib * GIB,
        },
    }
}

/// [`lease_ask`] made reclaimable.
pub(super) fn reclaimable(lease: u64, model: &str) -> LeaseAsk {
    LeaseAsk {
        reclaimable: true,
        ..lease_ask(lease, model)
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
    let _ = book.handle(Moment(now), finished(model));
}

/// mac-sessions' request for `model` ended.
pub(super) fn finished(model: &str) -> Event {
    Event::RequestFinished {
        model: m(model),
        client: ClientName::from("mac-sessions"),
    }
}

pub(super) fn forward(waiter: u64, model: &str) -> Action {
    Action::Forward {
        waiter: WaiterId(waiter),
        model: m(model),
        client: ClientName::from("mac-sessions"),
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
    Reason::Behind {
        model: m(model).into(),
    }
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
    let revoked = book
        .revoked
        .values()
        .filter(|revoked| revoked.counted)
        .map(|revoked| &revoked.lease);
    let bare_held: Vec<Footprint> = book
        .leases
        .values()
        .chain(revoked)
        .filter_map(|lease| lease.ask.bare())
        .collect();
    let bare_claimed: Vec<Footprint> = book
        .waiters
        .values()
        .filter_map(|waiter| waiter.lease.as_ref())
        .filter(|ask| book.claims.contains(&ask.lease))
        .filter_map(LeaseAsk::bare)
        .collect();
    let bare_later = [bare_held.clone(), bare_claimed.clone()].concat();
    for (set, holds) in [("now", &now as &dyn Fn(State) -> bool), ("later", &later)] {
        let held: Vec<_> = book.slots.iter().filter(|(_, s)| holds(s.state)).collect();
        let claimed = if set == "later" {
            &bare_claimed[..]
        } else {
            &[][..]
        };
        if !book.config.host.fits(
            held.iter()
                .map(|(_, s)| &s.footprint)
                .chain(&bare_held)
                .chain(claimed),
        ) {
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
    let fits_beside = |model: &ModelName, holds: &dyn Fn(State) -> bool, bare: &[Footprint]| {
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
                core::iter::once(wanted)
                    .chain(others.iter().map(|(_, slot)| &slot.footprint))
                    .chain(bare),
            )
    };
    let leaving = book
        .slots
        .values()
        .any(|slot| matches!(slot.state, State::Evicting | State::Unloading));
    let stranded = book.slots.iter().find(|(name, slot)| {
        slot.state == State::Reserved
            && fits_beside(name, &now, &bare_held)
            && fits_beside(name, &later, &bare_later)
    });
    match stranded {
        Some((name, _)) if !leaving => Some(format!("{name} is reserved beside free room")),
        _ => None,
    }
}

/// qwen's section of `HOST_AND_MODELS`, for tests that take it out.
pub(super) const QWEN_SECTION: &str = r#"[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
apis = ["openai"]
vram = "22323M"
ram = "4G"
idle = "2h"
"#;

pub(super) fn footprint(book: &Book, model: &str) -> Footprint {
    book.config.models[&m(model)].footprint
}

/// What discovery reports for `model` with no saved placement.
pub(super) fn found(model: &str, footprint: Footprint) -> Found {
    Found {
        model: m(model),
        footprint,
        placement: None,
        stray: false,
    }
}

pub(super) fn restored(ask: LeaseAsk, since: u64) -> RestoredLease {
    RestoredLease {
        ask,
        since: Moment(since),
        last_activity: None,
    }
}

pub(super) fn heartbeat(lease: u64, model: &str, ttl: u64) -> LeaseAsk {
    LeaseAsk {
        hold: Hold::Heartbeat {
            ttl: Duration::from_secs(ttl),
        },
        ..lease_ask(lease, model)
    }
}

pub(super) fn model_view(book: &Book, now: u64, model: &str) -> Option<ModelView> {
    let snapshot = book.snapshot(Moment(now));
    snapshot
        .models
        .into_iter()
        .find(|view| view.name == m(model))
}

pub(super) fn fail(waiter: u64, error: &str) -> Action {
    Action::Fail {
        waiter: WaiterId(waiter),
        error: error.to_owned(),
    }
}

pub(super) fn held_by_bench(model: &str, lease: u64, since: u64) -> Reason {
    Reason::Held {
        model: m(model).into(),
        client: ClientName::from("bench-01"),
        lease: LeaseId(lease),
        since: Moment(since),
        until: None,
        idle_since: Some(Moment(since)),
    }
}

pub(super) fn refuse(waiter: u64, reason: Reason) -> Action {
    Action::Refuse {
        waiter: WaiterId(waiter),
        refusal: Refusal {
            reason,
            retry_after: None,
        },
    }
}
