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
        model: m("iq2_xs").into(),
        client: ClientName::from("bench-01"),
        lease: LeaseId(lease),
        since: Moment(GRANTED),
        until: until.map(Moment),
        idle_since: Some(Moment(GRANTED)),
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
fn a_lease_past_its_expected_end_refuses_at_once_with_no_estimate() {
    let mut book = book();
    hold_iq2_xs(&mut book, Some(3_600));
    let actions = ask(
        &mut book,
        7_200_000,
        2,
        "qwen3.8:27b",
        Priority::Interactive,
    );
    assert_eq!(actions, vec![refuse(2, held(1, None), None)]);
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
fn a_renewal_after_the_ttl_is_too_late() {
    let mut book = book();
    let ttl = Duration::from_secs(60);
    hold_laya(&mut book, Hold::Heartbeat { ttl });
    let actions = lease_event(&mut book, 70_000, renewed);
    assert_eq!(actions, vec![ended(1, Ended::Expired), Action::Persist]);
    assert_eq!(book.lease(LeaseId(1)), None);
    assert_eq!(lease_event(&mut book, 70_000, released), vec![]);
}

#[test]
fn an_attach_after_the_reconnect_window_is_too_late() {
    let mut book = book();
    hold_laya(&mut book, Hold::Connection);
    let _ = lease_event(&mut book, 0, detached);
    let actions = lease_event(&mut book, 70_000, attached);
    assert_eq!(actions, vec![ended(1, Ended::Abandoned), Action::Persist]);
    assert_eq!(book.lease(LeaseId(1)), None);
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
        model: Some(m("laya")),
        footprint: None,
        pid: None,
        priority: Priority::Batch,
        since: Moment(10),
        expected_until: Some(Moment(3_600_010)),
        note: Some("strata h2h run 3".to_owned()),
        hold: Hold::Connection,
        attached: true,
        reclaimable: false,
        last_activity: Moment(10),
        in_use: false,
        release_if_idle: None,
        revoked: None,
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
fn a_held_model_that_fails_to_load_again_is_left_until_asked_for() {
    let mut book = book();
    hold_laya(&mut book, Hold::Connection);
    let laya = || m("laya");
    let failed = |error: &str| Event::LoadFailed {
        model: laya(),
        error: error.to_owned(),
    };
    let _ = book.handle(Moment(10), Event::BackendExited { model: laya() });
    let actions = book.handle(Moment(20), Event::Unloaded { model: laya() });
    assert_eq!(actions, vec![Action::Load(laya())]);
    assert_eq!(
        book.handle(Moment(30), failed("first")),
        vec![Action::Load(laya())]
    );

    assert_eq!(book.handle(Moment(40), failed("second")), vec![]);
    assert_eq!(book.state(&laya()), Some(State::Unloaded));
    assert!(book.lease(LeaseId(1)).is_some());
    assert_eq!(book.errors.back().map(|e| e.error.as_str()), Some("second"));
    assert_eq!(book.next_deadline(), None);
    assert_eq!(tick(&mut book, 3_600_000), vec![]);

    let actions = ask(&mut book, 3_600_010, 2, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(laya()),
            waiting_until(2, loading("laya"), 3_600_010),
        ]
    );

    // A load that works turns reloading back on for the lease.
    let actions = book.handle(Moment(3_600_020), Event::Loaded { model: laya() });
    assert_eq!(actions, vec![forward(2, "laya")]);
    let _ = book.handle(Moment(3_600_030), finished("laya"));
    let _ = book.handle(Moment(3_600_040), Event::BackendExited { model: laya() });
    let actions = book.handle(Moment(3_600_050), Event::Unloaded { model: laya() });
    assert_eq!(actions, vec![Action::Load(laya())]);
}

#[test]
fn a_crashed_held_model_keeps_its_room_while_it_unloads() {
    let mut book = book();
    hold_iq2_xs(&mut book, None);
    let iq2_xs = || m("iq2_xs");
    let actions = book.handle(Moment(60_000), Event::BackendExited { model: iq2_xs() });
    assert_eq!(actions, vec![Action::Unload(iq2_xs())]);
    let actions = ask(&mut book, 61_000, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![refuse(2, held(1, None), None)]);

    let actions = book.handle(Moment(62_000), Event::Unloaded { model: iq2_xs() });
    assert_eq!(actions, vec![Action::Load(iq2_xs())]);
    let actions = book.handle(Moment(70_000), Event::Loaded { model: iq2_xs() });
    assert_eq!(actions, vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
}

fn ask_seven_as_mac(book: &mut Book, now: u64) -> Vec<Action> {
    let ask = LeaseAsk {
        client: ClientName::from("mac-sessions"),
        ..lease_ask(7, "laya")
    };
    ask_lease(book, now, 2, ask)
}

#[test]
fn a_grant_on_a_live_lease_id_is_refused_and_the_first_lease_stands() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    assert_eq!(
        ask_lease(&mut book, 0, 1, lease_ask(7, "laya")),
        [grant(1, 7), Action::Persist]
    );

    let actions = ask_seven_as_mac(&mut book, 10);

    assert_eq!(actions, [fail(2, "lease 7 is already granted")]);
    let kept = book.lease(LeaseId(7)).expect("lease 7 stands");
    assert_eq!(kept.client, ClientName::from("bench-01"));
    assert_eq!(kept.since, Moment(0));
    assert_eq!(book.leases().len(), 1);
}

#[test]
fn a_fresh_grant_on_a_restored_lease_id_leaves_the_restored_one() {
    let mut book = book();
    let loaded = vec![found("laya", footprint(&book, "laya"))];
    let leases = vec![restored(lease_ask(7, "laya"), 0)];
    assert_eq!(book.restore(Moment(1_000), loaded, &[], leases), []);

    let actions = ask_seven_as_mac(&mut book, 2_000);

    assert_eq!(actions, [fail(2, "lease 7 is already granted")]);
    let kept = book.lease(LeaseId(7)).expect("lease 7 stands");
    assert_eq!(kept.client, ClientName::from("bench-01"));
    assert_eq!(kept.since, Moment(0));
}

#[test]
fn a_detach_on_a_heartbeat_lease_is_ignored() {
    let mut book = book();
    let ttl = Duration::from_secs(60);
    hold_laya(&mut book, Hold::Heartbeat { ttl });

    assert_eq!(lease_event(&mut book, 10_000, detached), vec![]);

    assert_eq!(book.lease(LeaseId(1)).map(|l| l.attached), Some(true));
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
}
