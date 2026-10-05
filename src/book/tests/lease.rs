use super::*;

const GRANTED: u64 = 50_000;

/// bench-01's lease 1 on iq2_xs, granted when iq2_xs loads at 50 s.
fn hold_iq2_xs(book: &mut Book, expected: Option<u64>) {
    let _ = take(book, 0, 1, 1, "iq2_xs", expected);
    let actions = book.handle(Moment(GRANTED), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(actions, vec![grant(1, 1), Action::Persist]);
}

fn held(lease: u64, until: Option<u64>) -> Reason {
    Reason::Held {
        model: m("iq2_xs"),
        client: ClientName::from("bench-01"),
        lease: LeaseId(lease),
        since: Moment(GRANTED),
        until: until.map(Moment),
    }
}

fn refuse(waiter: u64, reason: Reason, retry_after: Option<Duration>) -> Action {
    Action::Refuse {
        waiter: WaiterId(waiter),
        refusal: Refusal {
            reason,
            retry_after,
        },
    }
}

fn lease_event(book: &mut Book, now: u64, event: fn(LeaseId) -> Event) -> Vec<Action> {
    book.handle(Moment(now), event(LeaseId(1)))
}

fn released(lease: LeaseId) -> Event {
    Event::LeaseReleased { lease }
}

fn renewed(lease: LeaseId) -> Event {
    Event::LeaseRenewed { lease }
}

fn detached(lease: LeaseId) -> Event {
    Event::HolderDetached { lease }
}

fn attached(lease: LeaseId) -> Event {
    Event::HolderAttached { lease }
}

/// Lease 1 on a laya warmed at 0, granted at 0 with `hold`.
fn hold_laya(book: &mut Book, hold: Hold) {
    warm(book, 0, "laya");
    let ask = LeaseAsk {
        hold,
        ..lease_ask(1, "laya")
    };
    let actions = ask_lease(book, 0, 1, ask);
    assert_eq!(actions, vec![grant(1, 1), Action::Persist]);
}

#[test]
fn the_incident_a_held_strata_refuses_qwen_at_once() {
    let mut book = book();
    let actions = take(&mut book, 0, 1, 1, "iq2_xs", None);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq2_xs")),
            waiting_until(1, loading("iq2_xs"), 60_000),
        ]
    );
    let actions = book.handle(Moment(GRANTED), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(actions, vec![grant(1, 1), Action::Persist]);

    let actions = ask(
        &mut book,
        3_600_000,
        2,
        "qwen3.8:27b",
        Priority::Interactive,
    );
    assert_eq!(actions, vec![refuse(2, held(1, None), None)]);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Unloaded));
}

#[test]
fn a_request_waits_when_the_lease_ends_inside_its_cap() {
    let mut book = book();
    hold_iq2_xs(&mut book, Some(60));
    let actions = ask(&mut book, GRANTED, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(
        actions,
        vec![waiting_until(2, held(1, Some(110_000)), 110_000)]
    );

    let actions = lease_event(&mut book, 60_000, released);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("iq2_xs")),
            waiting(2, loading("qwen3.8:27b")),
            ended(1, Ended::Released),
            Action::Persist,
        ]
    );
    let actions = book.handle(Moment(61_000), Event::Unloaded { model: m("iq2_xs") });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("qwen3.8:27b")),
            waiting_until(2, loading("qwen3.8:27b"), 121_000),
        ]
    );
    let qwen = m("qwen3.8:27b");
    let actions = book.handle(Moment(70_000), Event::Loaded { model: qwen });
    assert_eq!(actions, vec![forward(2, "qwen3.8:27b")]);
}

#[test]
fn a_request_is_refused_when_the_lease_ends_past_its_cap() {
    let mut book = book();
    hold_iq2_xs(&mut book, Some(8 * 3_600));
    let actions = ask(&mut book, GRANTED, 2, "qwen3.8:27b", Priority::Interactive);
    let until = GRANTED + 8 * 3_600_000;
    let eight_hours = Some(Duration::from_secs(8 * 3_600));
    assert_eq!(actions, vec![refuse(2, held(1, Some(until)), eight_hours)]);
    assert_eq!(tick(&mut book, GRANTED + 1), vec![]);
}

#[test]
fn the_held_reason_names_the_lease_that_ends_last() {
    let mut book = book();
    hold_iq2_xs(&mut book, Some(60));
    let actions = take(&mut book, GRANTED, 2, 2, "iq2_xs", None);
    assert_eq!(actions, vec![grant(2, 2), Action::Persist]);

    let actions = ask(&mut book, GRANTED, 3, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![refuse(3, held(2, None), None)]);
}

#[test]
fn a_held_model_never_idle_unloads() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    assert_eq!(book.next_deadline(), None);
    assert_eq!(tick(&mut book, GRANTED + 2 * 3_600_000 + 1), vec![]);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
}

#[test]
fn a_released_lease_makes_its_model_reclaimable() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let actions = lease_event(&mut book, 100_000, released);
    assert_eq!(actions, vec![ended(1, Ended::Released), Action::Persist]);
    assert_eq!(book.lease(LeaseId(1)), None);
    // Only requests count as use: the load is the last one here.
    assert_eq!(book.slots[&m("iq2_xs")].last_used, Moment(GRANTED));

    let actions = ask(&mut book, 110_000, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("iq2_xs")),
            waiting(2, loading("qwen3.8:27b")),
        ]
    );
}

#[test]
fn a_heartbeat_lease_expires_after_its_ttl() {
    let mut book = book();
    let ttl = Duration::from_secs(60);
    hold_laya(&mut book, Hold::Heartbeat { ttl });
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
    assert_eq!(tick(&mut book, 59_000), vec![]);

    let actions = tick(&mut book, 60_000);
    assert_eq!(actions, vec![ended(1, Ended::Expired), Action::Persist]);
    assert_eq!(book.lease(LeaseId(1)), None);
}

#[test]
fn renewing_moves_the_expiry() {
    let mut book = book();
    let ttl = Duration::from_secs(60);
    hold_laya(&mut book, Hold::Heartbeat { ttl });
    assert_eq!(lease_event(&mut book, 50_000, renewed), vec![]);
    assert_eq!(tick(&mut book, 100_000), vec![]);
    assert!(book.lease(LeaseId(1)).is_some());

    let actions = tick(&mut book, 110_000);
    assert_eq!(actions, vec![ended(1, Ended::Expired), Action::Persist]);
}

#[test]
fn a_detached_lease_survives_the_reconnect_window() {
    let mut book = book();
    hold_laya(&mut book, Hold::Connection);
    assert_eq!(lease_event(&mut book, 0, detached), vec![]);
    assert_eq!(book.lease(LeaseId(1)).map(|l| l.attached), Some(false));
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
    assert_eq!(tick(&mut book, 59_000), vec![]);

    assert_eq!(lease_event(&mut book, 59_000, attached), vec![]);
    assert_eq!(book.lease(LeaseId(1)).map(|l| l.attached), Some(true));
    assert_eq!(book.next_deadline(), None);
    assert_eq!(tick(&mut book, 61_000), vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
}

#[test]
fn an_abandoned_lease_ends_after_the_reconnect_window() {
    let mut book = book();
    hold_laya(&mut book, Hold::Connection);
    let _ = lease_event(&mut book, 0, detached);
    // A second detach does not restart the window.
    assert_eq!(lease_event(&mut book, 30_000, detached), vec![]);

    let actions = tick(&mut book, 60_000);
    assert_eq!(actions, vec![ended(1, Ended::Abandoned), Action::Persist]);
    assert_eq!(book.leases(), vec![]);
}

#[test]
fn a_lease_on_a_loaded_model_is_granted_at_once() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let ask = LeaseAsk {
        expected: Some(Duration::from_secs(3_600)),
        note: Some("strata h2h run 3".to_owned()),
        ..lease_ask(1, "laya")
    };
    let actions = ask_lease(&mut book, 10, 1, ask);
    assert_eq!(actions, vec![grant(1, 1), Action::Persist]);

    let view = LeaseView {
        id: LeaseId(1),
        client: ClientName::from("bench-01"),
        model: m("laya"),
        since: Moment(10),
        expected_until: Some(Moment(3_600_010)),
        note: Some("strata h2h run 3".to_owned()),
        hold: Hold::Connection,
        attached: true,
    };
    assert_eq!(book.lease(LeaseId(1)), Some(view.clone()));
    assert_eq!(book.leases(), vec![view]);
}

#[test]
fn a_lease_with_max_wait_is_refused_like_a_request() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let ask = LeaseAsk {
        max_wait: Some(Duration::from_secs(10)),
        ..lease_ask(2, "qwen3.8:27b")
    };
    let actions = ask_lease(&mut book, 60_000, 2, ask);
    assert_eq!(actions, vec![refuse(2, held(1, None), None)]);
    assert_eq!(book.lease(LeaseId(2)), None);
}

#[test]
fn a_lease_without_max_wait_waits_behind_a_held_model() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let actions = ask_lease(&mut book, 60_000, 2, lease_ask(2, "qwen3.8:27b"));
    assert_eq!(actions, vec![waiting(2, held(1, None))]);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Unloaded));
    assert_eq!(tick(&mut book, 60_000 + 10 * 3_600_000), vec![]);
}

#[test]
fn a_released_model_unused_for_an_hour_is_evicted_by_a_batch_lease_at_once() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let released_at = GRANTED + 3_600_000;
    let actions = lease_event(&mut book, released_at, released);
    assert_eq!(actions, vec![ended(1, Ended::Released), Action::Persist]);

    let actions = take(&mut book, released_at + 10, 2, 2, "qwen3.8:27b", None);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("iq2_xs")),
            waiting(2, loading("qwen3.8:27b")),
        ]
    );
}

#[test]
fn a_release_after_the_idle_time_unloads_at_once() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let actions = lease_event(&mut book, GRANTED + 2 * 3_600_000, released);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("iq2_xs")),
            ended(1, Ended::Released),
            Action::Persist,
        ]
    );
}

#[test]
fn a_held_model_whose_backend_exits_loads_again_with_no_new_grant() {
    let mut book = book();
    hold_laya(&mut book, Hold::Connection);
    let actions = book.handle(Moment(10), Event::BackendExited { model: m("laya") });
    assert_eq!(actions, vec![Action::Unload(m("laya"))]);

    let actions = book.handle(Moment(20), Event::Unloaded { model: m("laya") });
    assert_eq!(actions, vec![Action::Load(m("laya"))]);
    let actions = book.handle(Moment(30), Event::Loaded { model: m("laya") });
    assert_eq!(actions, vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
    assert_eq!(tick(&mut book, 30 + 9 * 3_600_000), vec![]);
}

#[test]
fn a_crashed_held_model_waits_its_turn_to_load_again() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let iq2_xs = || m("iq2_xs");
    let qwen = || m("qwen3.8:27b");
    let actions = book.handle(Moment(60_000), Event::BackendExited { model: iq2_xs() });
    assert_eq!(actions, vec![Action::Unload(iq2_xs())]);
    let actions = ask(&mut book, 61_000, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![waiting(2, loading("qwen3.8:27b"))]);

    // qwen claimed the room first, so iq2_xs waits for it.
    let actions = book.handle(Moment(62_000), Event::Unloaded { model: iq2_xs() });
    assert_eq!(
        actions,
        vec![
            Action::Load(qwen()),
            waiting_until(2, loading("qwen3.8:27b"), 122_000),
        ]
    );
    let actions = book.handle(Moment(70_000), Event::Loaded { model: qwen() });
    assert_eq!(actions, vec![forward(2, "qwen3.8:27b")]);
    let actions = book.handle(Moment(70_000), Event::RequestFinished { model: qwen() });
    assert_eq!(actions, vec![]);

    // The lease is batch, so it waits out qwen's grace period.
    assert_eq!(book.next_deadline(), Some(Moment(190_000)));
    assert_eq!(tick(&mut book, 189_999), vec![]);
    assert_eq!(tick(&mut book, 190_000), vec![Action::Unload(qwen())]);
    let actions = book.handle(Moment(191_000), Event::Unloaded { model: qwen() });
    assert_eq!(actions, vec![Action::Load(iq2_xs())]);
    let actions = book.handle(Moment(200_000), Event::Loaded { model: iq2_xs() });
    assert_eq!(actions, vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
}
