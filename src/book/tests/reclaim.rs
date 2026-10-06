use super::*;

fn reclaimable(lease: u64, model: &str) -> LeaseAsk {
    LeaseAsk {
        reclaimable: true,
        ..lease_ask(lease, model)
    }
}

/// qwen warmed at 0 with bench-01's reclaimable lease 1 on it, granted at once.
fn keep_qwen(book: &mut Book) {
    warm(book, 0, QWEN);
    assert_eq!(
        ask_lease(book, 0, 1, reclaimable(1, QWEN)),
        vec![grant(1, 1), Action::Persist]
    );
}

#[test]
fn a_reclaimable_lease_keeps_its_model_past_its_idle_time() {
    let mut book = book();
    keep_qwen(&mut book);
    assert_eq!(tick(&mut book, 2 * 3_600_000 + 1), vec![]);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
    assert_eq!(
        book.next_deadline(),
        None,
        "no idle unload is due while the lease lasts"
    );
}

#[test]
fn an_interactive_waiter_evicts_it_and_the_lease_ends_reclaimed() {
    let mut book = book();
    keep_qwen(&mut book);
    let actions = ask(&mut book, 10, 2, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m(QWEN)),
            waiting(2, loading("iq2_xs")),
            ended(1, Ended::Reclaimed),
            Action::Persist,
        ]
    );
    assert!(book.leases().is_empty());
}

#[test]
fn a_batch_waiter_waits_out_the_grace_period_first() {
    let mut book = book();
    keep_qwen(&mut book);
    let actions = take(&mut book, 10, 2, 2, "iq2_xs", None);
    assert_eq!(
        actions,
        vec![waiting_until(
            2,
            Reason::Grace {
                model: m(QWEN),
                until: Moment(120_000)
            },
            180_000
        )]
    );
    let actions = tick(&mut book, 120_000);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m(QWEN)),
            waiting(2, loading("iq2_xs")),
            ended(1, Ended::Reclaimed),
            Action::Persist,
        ]
    );
}

/// Both leases name no end, so the higher id would be named if the reclaimable one counted.
#[test]
fn a_refusal_names_the_held_lease_and_never_the_reclaimable_one() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    assert_eq!(
        ask_lease(&mut book, 1, 1, lease_ask(1, QWEN)),
        vec![grant(1, 1), Action::Persist]
    );
    assert_eq!(
        ask_lease(&mut book, 5, 2, reclaimable(2, QWEN)),
        vec![grant(2, 2), Action::Persist]
    );
    let actions = ask(&mut book, 10, 3, "iq2_xs", Priority::Interactive);
    assert_eq!(actions, vec![refuse(3, held_by_bench(QWEN, 1, 1))]);
}

#[test]
fn a_model_named_by_a_held_and_a_reclaimable_lease_is_held() {
    let mut book = book();
    keep_qwen(&mut book);
    assert_eq!(
        ask_lease(&mut book, 5, 2, lease_ask(2, QWEN)),
        vec![grant(2, 2), Action::Persist]
    );
    let actions = ask(&mut book, 10, 3, "iq2_xs", Priority::Interactive);
    assert_eq!(actions, vec![refuse(3, held_by_bench(QWEN, 2, 5))]);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
}

#[test]
fn a_reclaimable_lease_loads_its_model_like_a_batch_lease() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let actions = ask_lease(&mut book, 200_000, 1, reclaimable(1, "iq2_xs"));
    assert_eq!(
        actions,
        vec![Action::Unload(m(QWEN)), waiting(1, loading("iq2_xs"))]
    );
    let actions = book.handle(Moment(200_010), Event::Unloaded { model: m(QWEN) });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq2_xs")),
            waiting_until(1, loading("iq2_xs"), 260_010)
        ]
    );
    let actions = book.handle(Moment(230_000), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(actions, vec![grant(1, 1), Action::Persist]);
}

#[test]
fn the_status_names_only_held_leases_as_holders() {
    let mut book = book();
    keep_qwen(&mut book);
    let view = model_view(&book, 10, QWEN).expect("qwen");
    assert_eq!(view.held_by, Vec::<ClientName>::new());
    assert!(book.leases()[0].reclaimable);
}

#[test]
fn a_reclaimable_models_backend_that_exits_ends_its_lease_reclaimed() {
    let mut book = book();
    keep_qwen(&mut book);
    assert_eq!(
        book.handle(Moment(10), Event::BackendExited { model: m(QWEN) }),
        vec![
            Action::Unload(m(QWEN)),
            ended(1, Ended::Reclaimed),
            Action::Persist
        ]
    );
    assert_eq!(
        book.handle(Moment(20), Event::Unloaded { model: m(QWEN) }),
        vec![],
        "nothing loads it again for a reclaimable lease"
    );
}

/// A held lease's crashed model claims room ahead of the queue; a reclaimable one's must not,
/// or the lease would hold up the waiter it promises never to block.
#[test]
fn a_waiter_is_not_held_up_by_a_reclaimable_models_crash() {
    let mut book = book();
    keep_qwen(&mut book);
    let _ = book.handle(Moment(10), Event::BackendExited { model: m(QWEN) });
    let actions = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);
    assert!(
        !actions.iter().any(|action| matches!(
            action,
            Action::Waiting {
                reason: Reason::Behind { .. },
                ..
            }
        )),
        "{actions:?}"
    );
    let actions = book.handle(Moment(30), Event::Unloaded { model: m(QWEN) });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq2_xs")),
            waiting_until(2, loading("iq2_xs"), 60_030)
        ]
    );
}

#[test]
fn a_release_after_the_idle_time_unloads_its_model_at_once() {
    let mut book = book();
    keep_qwen(&mut book);
    let actions = book.handle(
        Moment(2 * 3_600_000 + 1),
        Event::LeaseReleased { lease: LeaseId(1) },
    );
    assert_eq!(
        actions,
        vec![
            Action::Unload(m(QWEN)),
            ended(1, Ended::Released),
            Action::Persist,
        ]
    );
}

/// The eviction is committed at once and stands, so the lease ends then, while the model
/// drains its requests in flight, and the unload follows the last of them.
#[test]
fn an_eviction_ends_the_lease_before_requests_in_flight_drain() {
    let mut book = book();
    keep_qwen(&mut book);
    assert_eq!(
        ask(&mut book, 5, 2, QWEN, Priority::Interactive),
        vec![forward(2, QWEN)]
    );
    let actions = ask(&mut book, 10, 3, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            waiting(3, loading("iq2_xs")),
            ended(1, Ended::Reclaimed),
            Action::Persist,
        ]
    );
    assert_eq!(book.state(&m(QWEN)), Some(State::Evicting));
    assert!(book.leases().is_empty());
    let actions = book.handle(Moment(20), finished(QWEN));
    assert_eq!(actions, vec![Action::Unload(m(QWEN))]);
}

#[test]
fn a_reclaimable_ask_waits_out_grace_then_evicts_a_reclaimable_model() {
    let mut book = book();
    keep_qwen(&mut book);
    let actions = ask_lease(&mut book, 10, 2, reclaimable(2, "iq2_xs"));
    assert_eq!(
        actions,
        vec![waiting_until(
            2,
            Reason::Grace {
                model: m(QWEN),
                until: Moment(120_000)
            },
            180_000
        )]
    );
    let actions = tick(&mut book, 120_000);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m(QWEN)),
            waiting(2, loading("iq2_xs")),
            ended(1, Ended::Reclaimed),
            Action::Persist,
        ]
    );
}

#[test]
fn a_reclaimable_lease_that_asked_ends_idle_and_its_model_unloads_at_its_own_idle() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let ask = LeaseAsk {
        release_if_idle: Some(Duration::from_secs(1_800)),
        ..reclaimable(1, QWEN)
    };
    assert_eq!(
        ask_lease(&mut book, 0, 1, ask),
        vec![grant(1, 1), Action::Persist]
    );
    let idle = Ended::Idle {
        after: Duration::from_secs(1_800),
    };
    assert_eq!(
        tick(&mut book, 1_800_000),
        vec![ended(1, idle), Action::Persist]
    );
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
    assert_eq!(book.next_deadline(), Some(Moment(7_200_000)));
    assert_eq!(tick(&mut book, 7_200_000), vec![Action::Unload(m(QWEN))]);
}
