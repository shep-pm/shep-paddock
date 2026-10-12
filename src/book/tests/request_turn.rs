//! Requests for a model with turns: one in flight per turn, the rest queued.

use super::{
    turn::{holder, noted, one_turn, turns_toml},
    *,
};

const MAC: &str = "mac-sessions";
const BENCH: &str = "bench-01";

/// The reason a request waits on the request turn that `client`'s request holds
fn request_turn(client: &str, ahead: usize) -> Reason {
    Reason::Turn {
        model: m("iq3_s"),
        turn: TurnKind::Request,
        holders: vec![TurnHolder {
            client: ClientName::from(client),
            note: None,
            until: None,
        }],
        ahead,
    }
}

/// The reason a request waits on bench-01's lease `#79`, as a lease waiting for a turn would
fn lease_turn(ahead: usize) -> Reason {
    Reason::Turn {
        model: m("iq3_s"),
        turn: TurnKind::Lease,
        holders: vec![holder("#79", None)],
        ahead,
    }
}

/// A one-turn iq3_s, warm, with bench-01 holding lease 1 on it
fn leased() -> Book {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    book
}

fn bench_asks(book: &mut Book, now: u64, waiter: u64, priority: Priority) -> Vec<Action> {
    ask_as(book, now, waiter, BENCH, "iq3_s", priority)
}

fn forwarded(actions: &[Action]) -> Vec<u64> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Forward { waiter, .. } => Some(waiter.0),
            _ => None,
        })
        .collect()
}

#[test]
fn a_request_for_a_model_with_turns_is_forwarded_when_its_turn_is_free() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");

    let actions = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);

    assert_eq!(actions, vec![forward(1, "iq3_s")]);
}

#[test]
fn a_second_request_under_one_lease_waits_for_the_first_and_is_forwarded_when_it_finishes() {
    let mut book = leased();
    let first = bench_asks(&mut book, 1_000, 2, Priority::Interactive);

    let second = bench_asks(&mut book, 2_000, 3, Priority::Interactive);
    let done = book.handle(Moment(3_000), finished_as(2, BENCH, "iq3_s"));

    assert_eq!(first, vec![forward_as(2, BENCH, "iq3_s")]);
    assert_eq!(second, vec![waiting(3, lease_turn(0))]);
    assert_eq!(done, vec![forward_as(3, BENCH, "iq3_s")]);
}

#[test]
fn a_client_with_two_leases_gets_two_requests_in_flight() {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));

    let first = bench_asks(&mut book, 1_000, 3, Priority::Interactive);
    let second = bench_asks(&mut book, 1_000, 4, Priority::Interactive);
    let third = bench_asks(&mut book, 1_000, 5, Priority::Interactive);

    assert_eq!(first, vec![forward_as(3, BENCH, "iq3_s")]);
    assert_eq!(second, vec![forward_as(4, BENCH, "iq3_s")]);
    assert_eq!(forwarded(&third), Vec::<u64>::new());
    let done = book.handle(Moment(2_000), finished_as(4, BENCH, "iq3_s"));
    assert_eq!(forwarded(&done), vec![5]);
}

#[test]
fn a_client_whose_lease_turns_are_all_busy_waits_rather_than_taking_the_request_turn() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);

    let bench = bench_asks(&mut book, 2_000, 3, Priority::Interactive);
    let mac = ask(&mut book, 2_000, 4, "iq3_s", Priority::Interactive);

    assert_eq!(bench, vec![waiting(3, lease_turn(0))]);
    assert_eq!(mac, vec![forward(4, "iq3_s")], "the request turn was free");
}

#[test]
fn a_client_with_no_lease_takes_the_request_turn() {
    let mut book = leased();

    let first = ask(&mut book, 1_000, 2, "iq3_s", Priority::Interactive);
    let second = ask(&mut book, 2_000, 3, "iq3_s", Priority::Interactive);
    let third = ask(&mut book, 3_000, 4, "iq3_s", Priority::Interactive);

    assert_eq!(first, vec![forward(2, "iq3_s")]);
    assert_eq!(second, vec![waiting(3, request_turn(MAC, 0))]);
    assert_eq!(third, vec![waiting(4, request_turn(MAC, 1))]);
}

#[test]
fn a_client_with_only_a_reclaimable_lease_takes_the_request_turn() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, reclaimable(1, "iq3_s"));

    let first = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let second = bench_asks(&mut book, 2_000, 3, Priority::Interactive);

    assert_eq!(first, vec![forward_as(2, BENCH, "iq3_s")]);
    assert_eq!(second, vec![waiting(3, request_turn(BENCH, 0))]);
}

#[test]
fn the_request_turn_serves_interactive_before_batch_then_first_come() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    for (waiter, priority) in [
        (2, Priority::Batch),
        (3, Priority::Interactive),
        (4, Priority::Batch),
        (5, Priority::Interactive),
    ] {
        let _ = ask(&mut book, 2_000 + waiter, waiter, "iq3_s", priority);
    }

    let mut order = Vec::new();
    for finishing in [1, 3, 5, 2] {
        let actions = book.handle(Moment(10_000 + finishing), finished(finishing, "iq3_s"));
        order.extend(forwarded(&actions));
    }

    assert_eq!(order, vec![3, 5, 2, 4]);
}

#[test]
fn a_lease_turn_serves_its_holders_requests_first_come_whatever_their_priority() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let _ = bench_asks(&mut book, 2_000, 3, Priority::Batch);
    let _ = bench_asks(&mut book, 3_000, 4, Priority::Interactive);

    let first = book.handle(Moment(4_000), finished_as(2, BENCH, "iq3_s"));
    let second = book.handle(Moment(5_000), finished_as(3, BENCH, "iq3_s"));

    assert_eq!(forwarded(&first), vec![3]);
    assert_eq!(forwarded(&second), vec![4]);
}

#[test]
fn sequences_2_grants_two_leases_and_serves_three_conversations() {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let first = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let second = ask_lease(&mut book, 0, 2, noted(2, "#80"));
    let third_lease = ask_lease(&mut book, 0, 3, noted(3, "#81"));

    let under_one = bench_asks(&mut book, 1_000, 4, Priority::Interactive);
    let under_two = bench_asks(&mut book, 1_000, 5, Priority::Interactive);
    let strangers = ask(&mut book, 1_000, 6, "iq3_s", Priority::Interactive);
    let fourth = ask(&mut book, 1_000, 7, "iq3_s", Priority::Interactive);

    assert!(first.contains(&grant(1, 1)) && second.contains(&grant(2, 2)));
    assert!(!third_lease.contains(&grant(3, 3)), "{third_lease:?}");
    assert_eq!(forwarded(&under_one), vec![4]);
    assert_eq!(forwarded(&under_two), vec![5]);
    assert_eq!(forwarded(&strangers), vec![6]);
    assert_eq!(forwarded(&fourth), Vec::<u64>::new());
}

#[test]
fn a_waiting_request_is_told_what_it_waits_behind_and_its_place_in_line() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);

    let _ = ask(&mut book, 2_000, 2, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 3_000, 3, "iq3_s", Priority::Interactive);

    let waiters = book.snapshot(Moment(4_000)).waiters;
    let told: Vec<_> = waiters.iter().map(|waiter| waiter.reason.clone()).collect();
    assert_eq!(
        told,
        vec![Some(request_turn(MAC, 0)), Some(request_turn(MAC, 1)),]
    );
    assert!(waiters.iter().all(|w| w.kind == WaiterKind::Request));
}

#[test]
fn a_request_past_its_max_wait_is_refused_with_the_turn_reason() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 2_000, 2, "iq3_s", Priority::Interactive);

    let early = tick(&mut book, 121_999);
    let late = tick(&mut book, 122_000);

    assert_eq!(early, vec![]);
    assert_eq!(late, vec![refuse(2, request_turn(MAC, 0))]);
    assert!(book.snapshot(Moment(122_000)).waiters.is_empty());
}

#[test]
fn a_request_that_waited_for_a_load_is_admitted_against_turns_keeping_its_place() {
    let mut book = one_turn();
    let _ = ask(&mut book, 0, 1, "iq3_s", Priority::Batch);
    let _ = ask(&mut book, 10, 2, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 20, 3, "iq3_s", Priority::Interactive);

    let actions = book.handle(Moment(50_000), Event::Loaded { model: m("iq3_s") });

    assert_eq!(forwarded(&actions), vec![2]);
    assert!(
        actions.contains(&waiting(3, request_turn(MAC, 0))),
        "{actions:?}"
    );
    assert!(
        actions.contains(&waiting(1, request_turn(MAC, 1))),
        "{actions:?}"
    );
}

#[test]
fn a_model_without_sequences_forwards_every_request_at_once() {
    let mut book = book();
    warm(&mut book, 0, "iq3_s");

    let first = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    let second = ask(&mut book, 1_000, 2, "iq3_s", Priority::Batch);
    let third = bench_asks(&mut book, 1_000, 3, Priority::Interactive);

    assert_eq!(first, vec![forward(1, "iq3_s")]);
    assert_eq!(second, vec![forward(2, "iq3_s")]);
    assert_eq!(third, vec![forward_as(3, BENCH, "iq3_s")]);
}

#[test]
fn a_reload_that_removes_sequences_forwards_the_waiting_requests() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 2_000, 2, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 3_000, 3, "iq3_s", Priority::Interactive);

    let actions = book.reconfigure(
        Moment(4_000),
        test_support::config(test_support::HOST_AND_MODELS),
    );

    assert_eq!(forwarded(&actions), vec![2, 3]);
}

#[test]
fn a_reload_that_adds_sequences_queues_later_requests() {
    let mut book = book();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    let _ = book.reconfigure(Moment(2_000), test_support::config(&turns_toml(1)));

    let second = ask(&mut book, 3_000, 2, "iq3_s", Priority::Interactive);
    let third = ask(&mut book, 4_000, 3, "iq3_s", Priority::Interactive);

    assert_eq!(forwarded(&second), vec![2]);
    assert_eq!(third, vec![waiting(3, request_turn(MAC, 0))]);
}

#[test]
fn a_lease_ending_with_its_request_in_flight_leaves_a_waiting_request_on_the_request_turn() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let _ = bench_asks(&mut book, 2_000, 3, Priority::Interactive);

    let released = book.handle(Moment(3_000), Event::LeaseReleased { lease: LeaseId(1) });
    let later = bench_asks(&mut book, 4_000, 4, Priority::Interactive);
    let first_done = book.handle(Moment(5_000), finished_as(2, BENCH, "iq3_s"));
    let second_done = book.handle(Moment(6_000), finished_as(3, BENCH, "iq3_s"));

    assert_eq!(
        forwarded(&released),
        vec![3],
        "it took the free request turn"
    );
    assert_eq!(later, vec![waiting(4, request_turn(BENCH, 0))]);
    assert_eq!(forwarded(&first_done), Vec::<u64>::new());
    assert_eq!(forwarded(&second_done), vec![4]);
}

#[test]
fn a_request_whose_client_went_away_leaves_the_queue() {
    let mut book = one_turn();
    warm(&mut book, 0, "iq3_s");
    let _ = ask(&mut book, 1_000, 1, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 2_000, 2, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 3_000, 3, "iq3_s", Priority::Interactive);

    let gone = book.handle(
        Moment(4_000),
        Event::WaiterGone {
            waiter: WaiterId(2),
        },
    );
    let done = book.handle(Moment(5_000), finished(1, "iq3_s"));

    assert_eq!(gone, vec![waiting(3, request_turn(MAC, 0))]);
    assert_eq!(forwarded(&done), vec![3]);
}

#[test]
fn a_lease_waits_for_the_turn_of_an_ended_lease_whose_request_is_still_in_flight() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let _ = book.handle(Moment(2_000), Event::LeaseReleased { lease: LeaseId(1) });

    let asked = ask_lease(&mut book, 3_000, 3, noted(2, "#80"));
    let done = book.handle(Moment(4_000), finished_as(2, BENCH, "iq3_s"));

    let orphan = Reason::Turn {
        model: m("iq3_s"),
        turn: TurnKind::Lease,
        holders: vec![TurnHolder {
            client: ClientName::from(BENCH),
            note: None,
            until: None,
        }],
        ahead: 0,
    };
    assert_eq!(asked, vec![waiting(3, orphan)]);
    assert!(done.contains(&grant(3, 2)), "{done:?}");
}

#[test]
fn a_request_is_told_only_the_busy_turns_of_its_own_leases() {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let mac = LeaseAsk {
        client: ClientName::from(MAC),
        ..noted(2, "#80")
    };
    let _ = ask_lease(&mut book, 0, 2, mac);
    let _ = bench_asks(&mut book, 1_000, 3, Priority::Interactive);

    let second = bench_asks(&mut book, 2_000, 4, Priority::Interactive);

    assert_eq!(second, vec![waiting(4, lease_turn(0))]);
}

/// bench-01's request `waiter` for iq3_s, naming lease `lease` for its turn
fn bench_names(
    book: &mut Book,
    now: u64,
    waiter: u64,
    lease: u64,
    priority: Priority,
) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::RequestArrived {
            waiter: WaiterId(waiter),
            client: ClientName::from(BENCH),
            model: m("iq3_s"),
            priority,
            max_wait: Duration::from_secs(120),
            lease: Some(LeaseId(lease)),
        },
    )
}

/// A two-turn iq3_s, warm, with bench-01 holding leases 1 (`#79`) and 2 (`#80`) on it
fn leased_twice() -> Book {
    let mut book = book_from(&turns_toml(2));
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, noted(1, "#79"));
    let _ = ask_lease(&mut book, 0, 2, noted(2, "#80"));
    book
}

#[test]
fn a_request_naming_a_lease_takes_that_leases_turn_and_the_next_waits_for_it_alone() {
    let mut book = leased_twice();

    let first = bench_names(&mut book, 1_000, 3, 2, Priority::Interactive);
    let second = bench_names(&mut book, 2_000, 4, 2, Priority::Interactive);
    let unnamed = bench_asks(&mut book, 3_000, 5, Priority::Interactive);

    assert_eq!(first, vec![forward_as(3, BENCH, "iq3_s")]);
    let busy = Reason::Turn {
        model: m("iq3_s"),
        turn: TurnKind::Lease,
        holders: vec![holder("#80", None)],
        ahead: 0,
    };
    assert_eq!(second, vec![waiting(4, busy)], "lease 1 was free");
    assert_eq!(
        unnamed,
        vec![forward_as(5, BENCH, "iq3_s")],
        "it took lease 1's turn, which the waiting request does not want"
    );
}

#[test]
fn a_waiting_request_falls_through_when_its_lease_ends() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let waits = bench_names(&mut book, 2_000, 3, 1, Priority::Interactive);

    let released = book.handle(Moment(3_000), Event::LeaseReleased { lease: LeaseId(1) });

    assert_eq!(waits, vec![waiting(3, lease_turn(0))]);
    assert_eq!(
        forwarded(&released),
        vec![3],
        "it took the free request turn"
    );
}

#[test]
fn requests_naming_one_lease_take_its_turn_first_come_whatever_their_priority() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let _ = bench_names(&mut book, 2_000, 3, 1, Priority::Batch);
    let _ = bench_names(&mut book, 3_000, 4, 1, Priority::Interactive);

    let first = book.handle(Moment(4_000), finished_as(2, BENCH, "iq3_s"));
    let second = book.handle(Moment(5_000), finished_as(3, BENCH, "iq3_s"));

    assert_eq!(forwarded(&first), vec![3]);
    assert_eq!(forwarded(&second), vec![4]);
}

#[test]
fn a_request_with_no_free_turn_is_told_of_the_earlier_requests_naming_a_lease_ahead_of_it() {
    let mut book = leased_twice();
    let _ = bench_names(&mut book, 1_000, 3, 1, Priority::Interactive);
    let _ = bench_names(&mut book, 1_000, 4, 2, Priority::Interactive);
    let _ = bench_names(&mut book, 2_000, 5, 1, Priority::Interactive);

    let unnamed = bench_asks(&mut book, 3_000, 6, Priority::Interactive);

    let both = Reason::Turn {
        model: m("iq3_s"),
        turn: TurnKind::Lease,
        holders: vec![holder("#79", None), holder("#80", None)],
        ahead: 1,
    };
    assert_eq!(unnamed, vec![waiting(6, both)]);
}

/// The lease each waiter in the status is under, in queue order
fn leases_under(book: &Book, now: u64) -> Vec<Option<LeaseId>> {
    book.snapshot(Moment(now))
        .waiters
        .iter()
        .map(|waiter| waiter.lease)
        .collect()
}

#[test]
fn a_request_waiting_for_its_only_leases_busy_turn_is_under_that_lease() {
    let mut book = leased();
    let _ = bench_asks(&mut book, 1_000, 2, Priority::Interactive);
    let _ = bench_asks(&mut book, 2_000, 3, Priority::Interactive);

    assert_eq!(leases_under(&book, 3_000), [Some(LeaseId(1))]);
}

#[test]
fn a_request_naming_a_busy_lease_is_under_the_lease_it_names() {
    let mut book = leased_twice();
    let _ = bench_names(&mut book, 1_000, 3, 2, Priority::Interactive);
    let _ = bench_names(&mut book, 2_000, 4, 2, Priority::Interactive);

    assert_eq!(leases_under(&book, 3_000), [Some(LeaseId(2))]);
}

#[test]
fn a_request_waiting_for_the_first_of_several_busy_leases_is_under_none_of_them() {
    let mut book = leased_twice();
    let _ = bench_asks(&mut book, 1_000, 3, Priority::Interactive);
    let _ = bench_asks(&mut book, 2_000, 4, Priority::Interactive);
    let _ = bench_asks(&mut book, 3_000, 5, Priority::Interactive);

    assert_eq!(leases_under(&book, 4_000), [None]);
}

#[test]
fn a_request_on_the_request_turn_and_a_lease_waiter_are_under_no_lease() {
    let mut book = leased();
    let _ = ask(&mut book, 1_000, 2, "iq3_s", Priority::Interactive);
    let _ = ask(&mut book, 2_000, 3, "iq3_s", Priority::Interactive);
    let _ = ask_lease(&mut book, 3_000, 4, noted(4, "#81"));

    assert_eq!(leases_under(&book, 4_000), [None, None]);
}
