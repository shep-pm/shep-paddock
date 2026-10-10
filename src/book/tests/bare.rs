use super::*;

const EIGHT: Vram = Vram::Bytes(8 * GIB);

#[test]
fn a_bare_lease_that_fits_is_granted_at_once_and_counted() {
    let mut book = book();
    assert_eq!(
        ask_lease(&mut book, 0, 1, bare(1, EIGHT, 2)),
        vec![grant(1, 1), Action::Persist]
    );
    let view = book.lease(LeaseId(1)).expect("granted");
    assert_eq!(view.model, None);
    assert_eq!(
        view.footprint,
        Some(Footprint {
            vram: EIGHT,
            ram: 2 * GIB
        })
    );
    assert_eq!(view.pid, None);
    assert_eq!(
        book.snapshot(Moment(0)).declared,
        Footprint {
            vram: EIGHT,
            ram: 2 * GIB
        }
    );
}

#[test]
fn a_bare_lease_waits_for_a_held_model_and_is_told_so() {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, "iq2_xs", None);
    assert_eq!(
        book.handle(Moment(10), Event::Loaded { model: m("iq2_xs") }),
        vec![grant(1, 1), Action::Persist]
    );
    let actions = ask_lease(&mut book, 20, 2, bare(2, EIGHT, 2));
    assert_eq!(actions, vec![waiting(2, held_by_bench("iq2_xs", 1, 10))]);
}

#[test]
fn an_interactive_bare_lease_evicts_a_reclaimable_model_and_is_granted_once_it_leaves() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let ask = LeaseAsk {
        priority: Priority::Interactive,
        ..bare(1, EIGHT, 2)
    };
    let actions = ask_lease(&mut book, 10, 1, ask);
    let evicting = Reason::Evicting {
        model: m(QWEN),
        for_model: bare_taker(1, EIGHT, 2),
    };
    assert_eq!(actions, vec![Action::Unload(m(QWEN)), waiting(1, evicting)]);
    assert_eq!(
        book.handle(Moment(20), Event::Unloaded { model: m(QWEN) }),
        vec![grant(1, 1), Action::Persist]
    );
}

#[test]
fn a_batch_bare_lease_waits_out_the_grace_period_first() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let grace = Reason::Grace {
        model: m(QWEN),
        until: Moment(120_000),
    };
    assert_eq!(
        ask_lease(&mut book, 10, 1, bare(1, EIGHT, 2)),
        vec![waiting_until(1, grace, 120_000)],
        "a bare lease loads nothing, so its estimate is the grace's end"
    );
    let evicting = Reason::Evicting {
        model: m(QWEN),
        for_model: bare_taker(1, EIGHT, 2),
    };
    assert_eq!(
        tick(&mut book, 120_000),
        vec![Action::Unload(m(QWEN)), waiting(1, evicting)]
    );
}

#[test]
fn a_request_is_refused_naming_the_bare_lease_that_holds_its_room() {
    let mut book = book();
    let twenty = Vram::Bytes(20 * GIB);
    let _ = ask_lease(&mut book, 0, 1, bare(1, twenty, 4));
    let held = Reason::Held {
        model: bare_taker(1, twenty, 4),
        client: ClientName::from("bench-01"),
        lease: LeaseId(1),
        since: Moment(0),
        until: None,
        idle_since: None,
    };
    assert_eq!(
        ask(&mut book, 10, 2, QWEN, Priority::Interactive),
        vec![refuse(2, held)]
    );
}

#[test]
fn a_bare_lease_is_never_evicted_and_never_unloads_for_idleness() {
    let mut book = book();
    let _ = ask_lease(&mut book, 0, 1, bare(1, Vram::Bytes(2 * GIB), 1));
    let actions = ask(&mut book, 10, 2, "iq2_xs", Priority::Interactive);
    assert!(
        matches!(&actions[..], [Action::Refuse { refusal, .. }]
            if matches!(refusal.reason, Reason::Held { lease: LeaseId(1), .. })),
        "{actions:?}"
    );
    assert_eq!(book.next_deadline(), None);
    assert_eq!(tick(&mut book, 10 * 3_600_000), vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
}

#[test]
fn the_room_a_bare_lease_claims_holds_off_a_later_waiter() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let four = Vram::Bytes(4 * GIB);
    let claim = LeaseAsk {
        priority: Priority::Interactive,
        ..bare(1, four, 2)
    };
    let _ = ask_lease(&mut book, 10, 1, claim);
    let actions = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![waiting(
            2,
            Reason::Behind {
                model: bare_taker(1, four, 2)
            }
        )]
    );
    assert_eq!(broken(&book), None);
}

#[test]
fn a_bare_lease_for_all_the_vram_fits_beside_a_model_that_holds_only_ram() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    assert_eq!(
        ask_lease(&mut book, 10, 1, bare(1, Vram::All, 4)),
        vec![grant(1, 1), Action::Persist]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
    let actions = ask(&mut book, 20, 2, QWEN, Priority::Interactive);
    assert!(
        matches!(&actions[..], [Action::Refuse { refusal, .. }]
            if matches!(refusal.reason, Reason::Held { lease: LeaseId(1), .. })),
        "nothing is evicted for room it would not make: {actions:?}"
    );
}

#[test]
fn a_footprint_bigger_than_the_host_fails() {
    let mut book = book();
    assert_eq!(
        ask_lease(&mut book, 0, 1, bare(1, Vram::Bytes(25 * GIB), 1)),
        vec![fail(1, "the footprint cannot fit the host even when alone")]
    );
}

#[test]
fn a_released_bare_lease_frees_its_room_for_the_waiter_behind_it() {
    let mut book = book();
    let _ = ask_lease(&mut book, 0, 1, bare(1, Vram::Bytes(20 * GIB), 1));
    let _ = ask_lease(&mut book, 10, 2, bare(2, EIGHT, 1));
    assert_eq!(
        book.handle(Moment(20), Event::LeaseReleased { lease: LeaseId(1) }),
        vec![grant(2, 2), ended(1, Ended::Released), Action::Persist]
    );
}

#[test]
fn a_restored_bare_lease_counts_at_once_and_waits_detached_for_its_holder() {
    let mut book = book();
    let leases = vec![restored(bare(1, EIGHT, 2), 0)];
    assert_eq!(book.restore(Moment(1_000), Vec::new(), &[], leases), vec![]);
    assert_eq!(book.snapshot(Moment(1_000)).declared.ram, 2 * GIB);
    assert!(!book.lease(LeaseId(1)).expect("restored").attached);
    assert_eq!(
        book.next_deadline(),
        Some(Moment(61_000)),
        "its reconnect window"
    );
}

#[test]
fn a_stray_drops_a_bare_lease_claim_so_it_claims_again() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    // Beside the claim's 58G of RAM, laya's 5G no longer fits the host.
    let claim = LeaseAsk {
        priority: Priority::Interactive,
        ..bare(1, EIGHT, 58)
    };
    let _ = ask_lease(&mut book, 10, 1, claim);
    assert!(book.claims.contains(&LeaseId(1)));
    let laya = footprint(&book, "laya");
    let backend = book.config.models[&m("laya")].backend.clone();
    let actions = book.handle(
        Moment(20),
        Event::StrayFound {
            model: m("laya"),
            footprint: laya,
            backend,
        },
    );
    let evicting = Reason::Evicting {
        model: m("laya"),
        for_model: bare_taker(1, EIGHT, 58),
    };
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, evicting)],
        "claiming again under the stray, it evicts laya too"
    );
    assert!(book.claims.contains(&LeaseId(1)));
    assert_eq!(broken(&book), None);
}
