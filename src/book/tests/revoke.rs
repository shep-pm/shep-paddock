use super::*;
use crate::book::lease::Revocation;

const TWENTY: Vram = Vram::Bytes(20 * GIB);

fn by_mac(note: Option<&str>) -> Revocation {
    Revocation {
        by: ClientName::from("mac-sessions"),
        note: note.map(str::to_owned),
    }
}

fn revoked(lease: u64, note: Option<&str>) -> Event {
    Event::LeaseRevoked {
        lease: LeaseId(lease),
        by: ClientName::from("mac-sessions"),
        note: note.map(str::to_owned),
    }
}

#[test]
fn a_revoked_model_lease_ends_and_its_model_stays_loaded() {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, "iq2_xs", None);
    let _ = book.handle(Moment(10), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(
        book.handle(Moment(20), revoked(1, Some("forgotten since Tuesday"))),
        vec![
            ended(1, Ended::Revoked(by_mac(Some("forgotten since Tuesday")))),
            Action::Persist
        ]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
    assert!(
        book.snapshot(Moment(20)).leases.is_empty(),
        "a model lease is not listed once revoked"
    );
    let view = model_view(&book, 20, "iq2_xs").expect("iq2_xs");
    assert!(view.held_by.is_empty(), "reclaimable now");
}

#[test]
fn a_revoked_bare_lease_keeps_its_memory_counted_until_its_holder_detaches() {
    let mut book = book();
    let _ = ask_lease(&mut book, 0, 1, bare(1, TWENTY, 1));
    let blocked = Reason::Held {
        model: bare_taker(1, TWENTY, 1),
        client: ClientName::from("bench-01"),
        lease: LeaseId(1),
        since: Moment(0),
        until: None,
        idle_since: None,
    };
    assert_eq!(
        take(&mut book, 5, 2, 2, QWEN, None),
        vec![waiting(2, blocked)]
    );

    assert_eq!(
        book.handle(Moment(10), revoked(1, None)),
        vec![ended(1, Ended::Revoked(by_mac(None))), Action::Persist],
        "qwen's lease still waits on the same memory, so it is told nothing new"
    );
    let snapshot = book.snapshot(Moment(10));
    assert_eq!(snapshot.leases.len(), 1);
    assert_eq!(snapshot.leases[0].revoked, Some(by_mac(None)));
    assert_eq!(snapshot.declared.ram, GIB);

    assert_eq!(
        book.handle(Moment(20), Event::HolderDetached { lease: LeaseId(1) }),
        vec![
            Action::Load(m(QWEN)),
            waiting_until(2, loading(QWEN), 60_020)
        ]
    );
    assert!(book.snapshot(Moment(20)).leases.is_empty());
    assert_eq!(broken(&book), None);
}

#[test]
fn a_heartbeat_bare_lease_frees_its_memory_at_the_revoke_and_is_listed_until_its_ttl() {
    let mut book = book();
    let ask = LeaseAsk {
        hold: Hold::Heartbeat {
            ttl: Duration::from_secs(60),
        },
        ..bare(1, TWENTY, 1)
    };
    let _ = ask_lease(&mut book, 0, 1, ask);
    assert_eq!(
        book.handle(Moment(10), revoked(1, None)),
        vec![ended(1, Ended::Revoked(by_mac(None))), Action::Persist]
    );
    let snapshot = book.snapshot(Moment(10));
    assert_eq!(snapshot.declared.ram, 0);
    assert_eq!(snapshot.leases[0].revoked, Some(by_mac(None)));
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
    assert_eq!(tick(&mut book, 60_000), vec![]);
    assert!(book.snapshot(Moment(60_000)).leases.is_empty());
}

#[test]
fn a_bare_lease_whose_holder_had_detached_frees_its_memory_at_the_revoke() {
    let mut book = book();
    let _ = ask_lease(&mut book, 0, 1, bare(1, TWENTY, 1));
    let _ = book.handle(Moment(5), Event::HolderDetached { lease: LeaseId(1) });
    let _ = book.handle(Moment(10), revoked(1, None));
    assert_eq!(book.snapshot(Moment(10)).declared.ram, 0);
    assert_eq!(
        book.next_deadline(),
        Some(Moment(60_005)),
        "listed until its reconnect window would have closed"
    );
}

#[test]
fn a_revoked_lease_is_gone_for_every_later_call() {
    let mut book = book();
    let _ = ask_lease(&mut book, 0, 1, bare(1, TWENTY, 1));
    let _ = book.handle(Moment(10), revoked(1, None));
    assert_eq!(book.lease(LeaseId(1)), None);
    assert_eq!(book.handle(Moment(20), revoked(1, None)), vec![]);
    assert_eq!(
        book.handle(Moment(20), Event::LeaseReleased { lease: LeaseId(1) }),
        vec![]
    );
}

#[test]
fn revoking_a_lease_nobody_holds_does_nothing() {
    let mut book = book();
    assert_eq!(book.handle(Moment(0), revoked(9, None)), vec![]);
}
