use super::*;

const BENCH: &str = "bench-01";

/// bench-01's lease 1 on a laya warmed at 0, granted at 0, with `hold` and `idle` seconds.
fn idle_laya(book: &mut Book, hold: Hold, idle: Option<u64>) {
    warm(book, 0, "laya");
    let ask = LeaseAsk {
        hold,
        release_if_idle: idle.map(Duration::from_secs),
        ..lease_ask(1, "laya")
    };
    assert_eq!(
        ask_lease(book, 0, 1, ask),
        vec![grant(1, 1), Action::Persist]
    );
}

fn as_bench(book: &mut Book, now: u64, waiter: u64, model: &str) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::RequestArrived {
            waiter: WaiterId(waiter),
            client: ClientName::from(BENCH),
            model: m(model),
            priority: Priority::Interactive,
            max_wait: Duration::from_secs(120),
        },
    )
}

fn bench_finished(book: &mut Book, now: u64, model: &str) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::RequestFinished {
            model: m(model),
            client: ClientName::from(BENCH),
        },
    )
}

fn note(book: &mut Book, now: u64, text: &str) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::LeaseNoted {
            lease: LeaseId(1),
            note: text.to_owned(),
        },
    )
}

fn lease_1(book: &Book) -> LeaseView {
    book.lease(LeaseId(1)).expect("lease 1 is granted")
}

fn idle_after(seconds: u64) -> Ended {
    Ended::Idle {
        after: Duration::from_secs(seconds),
    }
}

#[test]
fn a_lease_starts_active_at_its_grant() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(0), false));
}

#[test]
fn a_request_from_the_holder_is_activity_and_use_until_it_ends() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    assert_eq!(
        as_bench(&mut book, 1_000, 2, "laya"),
        vec![Action::Forward {
            waiter: WaiterId(2),
            model: m("laya"),
            client: ClientName::from(BENCH)
        }]
    );
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(1_000), true));

    assert_eq!(bench_finished(&mut book, 5_000, "laya"), vec![]);
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(5_000), false));
}

#[test]
fn a_request_from_another_client_or_for_another_model_is_not_activity() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    idle_laya(&mut book, Hold::Connection, Some(60));
    assert_eq!(
        ask(&mut book, 1_000, 2, "laya", Priority::Interactive),
        vec![forward(2, "laya")]
    );
    assert!(
        !lease_1(&book).in_use,
        "mac-sessions' request is not bench-01's"
    );
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
    let _ = book.handle(Moment(1_500), finished("laya"));
    assert_eq!(
        as_bench(&mut book, 2_000, 3, QWEN),
        vec![Action::Forward {
            waiter: WaiterId(3),
            model: m(QWEN),
            client: ClientName::from(BENCH),
        }]
    );
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(0), false));
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
}

#[test]
fn a_note_is_activity_and_replaces_the_note() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    assert_eq!(
        note(&mut book, 2_000, "step 412/900"),
        vec![Action::Persist]
    );
    let view = lease_1(&book);
    assert_eq!(view.last_activity, Moment(2_000));
    assert_eq!(view.note.as_deref(), Some("step 412/900"));
}

#[test]
fn a_note_renews_a_heartbeat_lease() {
    let mut book = book();
    idle_laya(
        &mut book,
        Hold::Heartbeat {
            ttl: Duration::from_secs(60),
        },
        None,
    );
    let _ = note(&mut book, 50_000, "step 1");
    assert_eq!(tick(&mut book, 100_000), vec![]);
    assert_eq!(
        tick(&mut book, 110_000),
        vec![ended(1, Ended::Expired), Action::Persist]
    );
}

/// A hung benchmark still heartbeats, so a renewal alone must not keep it in use.
#[test]
fn a_plain_renewal_is_not_activity() {
    let mut book = book();
    idle_laya(
        &mut book,
        Hold::Heartbeat {
            ttl: Duration::from_secs(60),
        },
        Some(90),
    );
    let _ = book.handle(Moment(50_000), Event::LeaseRenewed { lease: LeaseId(1) });
    assert_eq!(tick(&mut book, 89_999), vec![]);
    assert_eq!(
        tick(&mut book, 90_000),
        vec![ended(1, idle_after(90)), Action::Persist]
    );
}

#[test]
fn a_lease_that_asked_ends_once_it_has_been_idle_that_long() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, Some(1_800));
    assert_eq!(book.next_deadline(), Some(Moment(1_800_000)));
    assert_eq!(tick(&mut book, 1_799_999), vec![]);
    assert_eq!(
        tick(&mut book, 1_800_000),
        vec![ended(1, idle_after(1_800)), Action::Persist]
    );
    assert_eq!(
        book.state(&m("laya")),
        Some(State::Loaded),
        "the model stays until its own idle"
    );
}

#[test]
fn a_lease_that_did_not_ask_is_never_released_for_idleness() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    assert_eq!(tick(&mut book, 36_000_000), vec![]);
    assert!(book.lease(LeaseId(1)).is_some());
}

#[test]
fn a_request_in_flight_keeps_its_lease_from_going_idle() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, Some(60));
    let _ = as_bench(&mut book, 10_000, 2, "laya");
    assert_eq!(
        book.next_deadline(),
        None,
        "no idle end while the request runs"
    );
    assert_eq!(tick(&mut book, 200_000), vec![]);
    assert!(lease_1(&book).in_use);

    let _ = bench_finished(&mut book, 300_000, "laya");
    assert_eq!(book.next_deadline(), Some(Moment(360_000)));
    assert_eq!(
        tick(&mut book, 360_000),
        vec![ended(1, idle_after(60)), Action::Persist]
    );
}

#[test]
fn a_held_reason_says_since_when_its_lease_has_been_idle() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    let _ = note(&mut book, 5_000, "step 1");
    let held = |idle_since: Option<u64>| Reason::Held {
        model: m("laya").into(),
        client: ClientName::from(BENCH),
        lease: LeaseId(1),
        since: Moment(0),
        until: None,
        idle_since: idle_since.map(Moment),
    };
    assert_eq!(
        ask(&mut book, 6_000, 2, "iq3_s", Priority::Interactive),
        vec![refuse(2, held(Some(5_000)))]
    );

    let _ = as_bench(&mut book, 7_000, 3, "laya");
    assert_eq!(
        ask(&mut book, 8_000, 4, "iq3_s", Priority::Interactive),
        vec![refuse(4, held(None))]
    );
}

#[test]
fn an_idle_release_lets_a_waiting_lease_in() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, Some(60));
    let waiting_ask = LeaseAsk {
        priority: Priority::Interactive,
        ..lease_ask(2, "iq3_s")
    };
    let held = Reason::Held {
        model: m("laya").into(),
        client: ClientName::from(BENCH),
        lease: LeaseId(1),
        since: Moment(0),
        until: None,
        idle_since: Some(Moment(0)),
    };
    assert_eq!(
        ask_lease(&mut book, 1_000, 2, waiting_ask),
        vec![waiting(2, held)]
    );
    assert_eq!(
        tick(&mut book, 60_000),
        vec![
            Action::Unload(m("laya")),
            waiting(2, loading("iq3_s")),
            ended(1, idle_after(60)),
            Action::Persist,
        ]
    );
}

#[test]
fn a_restored_lease_keeps_its_saved_activity_or_starts_at_the_restart() {
    for (last_activity, idle_end) in [(Some(100), 60_100), (None, 61_000)] {
        let mut book = book();
        let ask = LeaseAsk {
            hold: Hold::Heartbeat {
                ttl: Duration::from_secs(120),
            },
            release_if_idle: Some(Duration::from_secs(60)),
            ..lease_ask(1, "laya")
        };
        let lease = RestoredLease {
            ask,
            since: Moment(0),
            last_activity: last_activity.map(Moment),
        };
        let _ = book.restore(
            Moment(1_000),
            vec![found("laya", footprint(&book, "laya"))],
            &[],
            vec![lease],
        );
        assert_eq!(tick(&mut book, idle_end - 1), vec![], "{last_activity:?}");
        assert_eq!(
            tick(&mut book, idle_end),
            vec![ended(1, idle_after(60)), Action::Persist],
            "{last_activity:?}"
        );
    }
}

#[test]
fn a_lease_past_two_ends_at_once_ends_for_the_earlier() {
    for (ttl, idle, why) in [(60, 90, Ended::Expired), (120, 30, idle_after(30))] {
        let mut book = book();
        let hold = Hold::Heartbeat {
            ttl: Duration::from_secs(ttl),
        };
        idle_laya(&mut book, hold, Some(idle));
        assert_eq!(
            tick(&mut book, 200_000),
            vec![ended(1, why), Action::Persist],
            "{why:?}"
        );
    }
}

/// A benchmark sends hundreds of requests, and each would otherwise re-tell every waiter.
#[test]
fn the_holders_use_tells_a_waiter_nothing_new_but_shows_in_the_status() {
    let mut book = book();
    idle_laya(&mut book, Hold::Connection, None);
    let waiting_ask = LeaseAsk {
        priority: Priority::Interactive,
        ..lease_ask(2, "iq3_s")
    };
    let _ = ask_lease(&mut book, 1_000, 2, waiting_ask);
    for i in 0..5 {
        let at = 2_000 + i * 1_000;
        assert_eq!(
            as_bench(&mut book, at, 10 + i, "laya"),
            vec![Action::Forward {
                waiter: WaiterId(10 + i),
                model: m("laya"),
                client: ClientName::from(BENCH),
            }]
        );
        assert_eq!(bench_finished(&mut book, at + 500, "laya"), vec![]);
    }
    assert_eq!(note(&mut book, 8_000, "step 2"), vec![Action::Persist]);

    let told = book.snapshot(Moment(8_000)).waiters[0].reason.clone();
    assert!(
        matches!(
            told,
            Some(Reason::Held {
                idle_since: Some(Moment(8_000)),
                ..
            })
        ),
        "{told:?}"
    );
}

/// laya, held by lease 1 with a 60 s idle limit, crashes at 1 s and starts loading again at 2 s.
fn reload_laya(book: &mut Book) {
    idle_laya(book, Hold::Connection, Some(60));
    let _ = book.handle(Moment(1_000), Event::BackendExited { model: m("laya") });
    assert_eq!(
        book.handle(Moment(2_000), Event::Unloaded { model: m("laya") }),
        vec![Action::Load(m("laya"))]
    );
}

#[test]
fn a_holders_request_waiting_for_the_model_keeps_its_lease_from_going_idle() {
    let mut book = book();
    reload_laya(&mut book);
    let _ = as_bench(&mut book, 3_000, 2, "laya");
    assert!(lease_1(&book).in_use);
    assert_eq!(tick(&mut book, 63_000), vec![]);
    assert!(book.lease(LeaseId(1)).is_some());

    let _ = book.handle(Moment(100_000), Event::Loaded { model: m("laya") });
    let _ = bench_finished(&mut book, 101_000, "laya");
    assert_eq!(book.next_deadline(), Some(Moment(161_000)));
}

#[test]
fn a_holders_request_that_leaves_the_queue_unserved_has_ended() {
    let mut book = book();
    reload_laya(&mut book);
    let _ = as_bench(&mut book, 3_000, 2, "laya");
    let gone = Event::WaiterGone {
        waiter: WaiterId(2),
    };
    assert_eq!(book.handle(Moment(50_000), gone), vec![]);
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(50_000), false));
    assert_eq!(book.next_deadline(), Some(Moment(110_000)));
}

#[test]
fn a_holders_request_refused_from_the_queue_has_ended() {
    let mut book = book();
    reload_laya(&mut book);
    let _ = as_bench(&mut book, 3_000, 2, "laya");
    let actions = tick(&mut book, 123_000);
    assert!(
        matches!(
            actions.as_slice(),
            [Action::Refuse {
                waiter: WaiterId(2),
                ..
            }]
        ),
        "{actions:?}"
    );
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(123_000), false));
}

#[test]
fn a_holders_request_failed_from_the_queue_has_ended() {
    let mut book = book();
    reload_laya(&mut book);
    let _ = as_bench(&mut book, 3_000, 2, "laya");
    let failed = || Event::LoadFailed {
        model: m("laya"),
        error: "out of memory".to_owned(),
    };
    assert_eq!(
        book.handle(Moment(100_000), failed()),
        vec![
            Action::Load(m("laya")),
            waiting_until(2, loading("laya"), 100_000)
        ]
    );
    assert_eq!(
        book.handle(Moment(100_500), failed()),
        vec![fail(2, "out of memory")]
    );
    let view = lease_1(&book);
    assert_eq!((view.last_activity, view.in_use), (Moment(100_500), false));
    assert_eq!(book.next_deadline(), Some(Moment(160_500)));
}

/// Its model is not loaded, so a lease kept for one more step would load it for nobody.
#[test]
fn a_lease_restored_past_its_idle_end_ends_at_the_restore() {
    let mut book = book();
    let ask = LeaseAsk {
        release_if_idle: Some(Duration::from_secs(60)),
        ..lease_ask(1, "laya")
    };
    let lease = RestoredLease {
        ask,
        since: Moment(0),
        last_activity: Some(Moment(100)),
    };
    assert_eq!(
        book.restore(Moment(100_000), Vec::new(), &[], vec![lease]),
        vec![ended(1, idle_after(60)), Action::Persist]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
}
