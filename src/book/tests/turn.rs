//! A model whose backend serves a limited number of leases at once.

use super::*;

const IQ3_S_TAIL: &str = "excludes = [\"laya\"]\nidle = \"2h\"";

/// The test host with iq3_s serving `sequences` leases at once.
fn turns_toml(sequences: u32) -> String {
    assert!(
        test_support::HOST_AND_MODELS.contains(IQ3_S_TAIL),
        "iq3_s's section moved"
    );
    test_support::HOST_AND_MODELS.replace(
        IQ3_S_TAIL,
        &format!("{IQ3_S_TAIL}\nsequences = {sequences}"),
    )
}

/// The test host with iq3_s serving one lease at a time.
fn one_turn() -> Book {
    book_from(&turns_toml(1))
}

/// bench-01's lease `lease` on iq3_s, noted `note`.
fn noted(lease: u64, note: &str) -> LeaseAsk {
    LeaseAsk {
        note: Some(note.to_owned()),
        ..lease_ask(lease, "iq3_s")
    }
}

fn holder(note: &str, until: Option<u64>) -> TurnHolder {
    TurnHolder {
        client: ClientName::from("bench-01"),
        note: Some(note.to_owned()),
        until: until.map(Moment),
    }
}

fn turn(holders: Vec<TurnHolder>, ahead: usize) -> Reason {
    Reason::Turn {
        model: m("iq3_s"),
        holders,
        ahead,
    }
}

#[test]
fn a_lease_past_the_limit_waits_for_a_turn_and_gets_the_next_one() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    assert_eq!(
        ask_lease(&mut book, 0, 1, noted(1, "#79")),
        vec![grant(1, 1), Action::Persist]
    );

    let actions = ask_lease(&mut book, 1_000, 2, noted(2, "#80"));

    assert_eq!(
        actions,
        vec![waiting(2, turn(vec![holder("#79", None)], 0))]
    );
    let actions = book.handle(Moment(2_000), Event::LeaseReleased { lease: LeaseId(1) });
    assert!(actions.contains(&grant(2, 2)), "{actions:?}");
}

#[test]
fn a_turn_waiter_is_told_its_place_and_the_first_its_estimate() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let first = LeaseAsk {
        expected: Some(Duration::from_secs(600)),
        ..noted(1, "#79")
    };
    let _ = ask_lease(&mut book, 0, 1, first);
    let holders = || vec![holder("#79", Some(600_000))];

    let second = ask_lease(&mut book, 1_000, 2, noted(2, "#80"));
    let third = ask_lease(&mut book, 2_000, 3, noted(3, "#81"));

    assert_eq!(second, vec![waiting_until(2, turn(holders(), 0), 600_000)]);
    assert_eq!(third, vec![waiting(3, turn(holders(), 1))]);
}

#[test]
fn a_reclaimable_lease_takes_no_turn_and_waits_for_none() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, reclaimable(1, "iq3_s"));

    let held = ask_lease(&mut book, 0, 2, noted(2, "#79"));
    let reclaimable = ask_lease(&mut book, 0, 3, reclaimable(3, "iq3_s"));

    assert_eq!(held, vec![grant(2, 2), Action::Persist]);
    assert_eq!(reclaimable, vec![grant(3, 3), Action::Persist]);
}

#[test]
fn a_request_is_forwarded_whoever_takes_the_turns() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));

    let actions = ask(&mut book, 1_000, 2, "iq3_s", Priority::Interactive);

    assert_eq!(actions, vec![forward(2, "iq3_s")]);
}

#[test]
fn without_sequences_every_lease_is_granted() {
    let mut book = book();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));

    let actions = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    assert_eq!(actions, vec![grant(2, 2), Action::Persist]);
}

/// Both asked while iq3_s was unloaded, so the load serves only the first.
#[test]
fn a_load_grants_one_turn_and_tells_the_next_lease_why_it_waits() {
    let mut book = one_turn();
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let actions = book.handle(Moment(50_000), Event::Loaded { model: m("iq3_s") });

    assert!(actions.contains(&grant(1, 1)), "{actions:?}");
    assert!(
        actions.contains(&waiting(2, turn(vec![holder("#79", None)], 0))),
        "{actions:?}"
    );
    assert!(!actions.contains(&grant(2, 2)), "{actions:?}");
}

#[test]
fn an_interactive_lease_takes_the_next_turn_ahead_of_a_batch_one() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 1_000, 2, noted(2, "batch"));
    let interactive = LeaseAsk {
        priority: Priority::Interactive,
        ..noted(3, "interactive")
    };
    let _ = ask_lease(&mut book, 2_000, 3, interactive);

    let actions = book.handle(Moment(3_000), Event::LeaseReleased { lease: LeaseId(1) });

    assert!(actions.contains(&grant(3, 3)), "{actions:?}");
    assert!(!actions.contains(&grant(2, 2)), "{actions:?}");
}

fn with_heartbeat(ask: LeaseAsk, ttl: u64) -> LeaseAsk {
    LeaseAsk {
        hold: Hold::Heartbeat {
            ttl: Duration::from_secs(ttl),
        },
        ..ask
    }
}

fn expecting(ask: LeaseAsk, seconds: u64) -> LeaseAsk {
    LeaseAsk {
        expected: Some(Duration::from_secs(seconds)),
        ..ask
    }
}

/// Lease 1's heartbeat runs out at 10 s. The ask that comes first after
/// that frees its turn, and still goes behind the lease queued before it.
#[test]
fn a_new_ask_never_takes_a_freed_turn_ahead_of_one_queued() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, with_heartbeat(noted(1, "#79"), 10));
    let _ = ask_lease(&mut book, 1_000, 2, noted(2, "#80"));

    let actions = ask_lease(&mut book, 10_500, 3, noted(3, "#81"));

    assert!(actions.contains(&grant(2, 2)), "{actions:?}");
    assert!(
        actions.contains(&waiting(3, turn(vec![holder("#80", None)], 0))),
        "{actions:?}"
    );
}

#[test]
fn a_refused_waiter_ahead_moves_the_next_one_up() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let capped = LeaseAsk {
        max_wait: Some(Duration::from_secs(5)),
        ..noted(2, "#80")
    };
    let _ = ask_lease(&mut book, 0, 2, capped);
    let _ = ask_lease(&mut book, 0, 3, noted(3, "#81"));

    let actions = tick(&mut book, 6_000);

    assert!(
        actions.iter().any(|action| matches!(
            action,
            Action::Refuse { waiter, .. } if *waiter == WaiterId(2)
        )),
        "{actions:?}"
    );
    assert!(
        actions.contains(&waiting(3, turn(vec![holder("#79", None)], 0))),
        "{actions:?}"
    );
}

/// A reload lowered the limit under two holders, so both must end before the next turn.
#[test]
fn a_turn_past_a_lowered_limit_waits_for_as_many_holders_as_it_must() {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, expecting(noted(1, "#79"), 600));
    let _ = ask_lease(&mut book, 0, 2, expecting(noted(2, "#80"), 1_200));
    let _ = book.reconfigure(Moment(0), test_support::config(&turns_toml(1)));

    let actions = ask_lease(&mut book, 1_000, 3, noted(3, "#81"));

    let holders = vec![holder("#79", Some(600_000)), holder("#80", Some(1_200_000))];
    assert_eq!(actions, vec![waiting_until(3, turn(holders, 0), 1_200_000)]);
}

#[test]
fn a_reload_with_a_higher_limit_grants_the_waiting_turn() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let actions = book.reconfigure(Moment(1_000), test_support::config(&turns_toml(2)));

    assert!(actions.contains(&grant(2, 2)), "{actions:?}");
}

#[test]
fn two_turns_take_two_leases_and_name_both_holders_to_the_third() {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let first = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let second = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let third = ask_lease(&mut book, 0, 3, noted(3, "#81"));

    assert!(first.contains(&grant(1, 1)) && second.contains(&grant(2, 2)));
    let holders = vec![holder("#79", None), holder("#80", None)];
    assert_eq!(third, vec![waiting(3, turn(holders, 0))]);
}

#[test]
fn a_holder_whose_heartbeat_runs_out_frees_its_turn_on_the_tick() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, with_heartbeat(noted(1, "#79"), 10));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let actions = tick(&mut book, 10_000);

    assert!(actions.contains(&grant(2, 2)), "{actions:?}");
}

#[test]
fn the_first_in_line_going_away_tells_the_next_its_place_and_estimate() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, expecting(noted(1, "#79"), 600));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));
    let _ = ask_lease(&mut book, 0, 3, noted(3, "#81"));

    let actions = book.handle(
        Moment(1_000),
        Event::WaiterGone {
            waiter: WaiterId(2),
        },
    );

    let holders = vec![holder("#79", Some(600_000))];
    assert_eq!(actions, vec![waiting_until(3, turn(holders, 0), 600_000)]);
}

/// The holder keeps its turn while its model loads again, so the waiter is not told the load.
#[test]
fn a_turn_waiter_still_waits_its_turn_while_the_holders_model_reloads() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let exited = book.handle(Moment(1_000), Event::BackendExited { model: m("iq3_s") });
    let reloaded = book.handle(Moment(50_000), Event::Loaded { model: m("iq3_s") });

    let told_2 = |actions: &[Action]| {
        actions.iter().any(|action| {
            matches!(action, Action::Waiting { waiter, .. } | Action::Grant { waiter, .. }
                if *waiter == WaiterId(2))
        })
    };
    assert!(!told_2(&exited), "{exited:?}");
    assert!(!told_2(&reloaded), "{reloaded:?}");
}

/// Both asked while iq3_s was unloaded. Its load serves only the first, so the second gets no
/// estimate from it.
#[test]
fn a_lease_past_the_free_turns_gets_no_estimate_from_the_load() {
    let mut book = one_turn();
    let first = ask_lease(&mut book, 0, 1, noted(1, "#79"));

    let second = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    assert!(
        first.contains(&waiting_until(1, loading("iq3_s"), 60_000)),
        "{first:?}"
    );
    assert_eq!(second, vec![waiting(2, loading("iq3_s"))]);
}

/// A holder's progress notes change the turn reason, but the waiter is told only once.
#[test]
fn a_holders_note_is_not_told_to_its_turn_waiters() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let actions = book.handle(
        Moment(1_000),
        Event::LeaseNoted {
            lease: LeaseId(1),
            note: "step 2".to_owned(),
        },
    );

    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Waiting { .. })),
        "{actions:?}"
    );
}
