use super::*;

// Three 10G models on a 24G card: any two fit, all three do not.
const THREE_EVEN_MODELS: &str = r#"
[host]
vram = "24G"
ram = "32G"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.a]
backend = "ollama"
name = "a"
vram = "10G"
ram = "1G"
idle = "1h"

[models.b]
backend = "ollama"
name = "b"
vram = "10G"
ram = "1G"
idle = "1h"

[models.c]
backend = "ollama"
name = "c"
vram = "10G"
ram = "1G"
idle = "1h"
"#;

fn grace(model: &str, until: u64) -> Reason {
    Reason::Grace {
        model: m(model),
        until: Moment(until),
    }
}

#[test]
fn interactive_goes_before_batch() {
    let mut book = book();
    let actions = take(&mut book, 0, 1, 1, "laya", None);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_000),
        ]
    );
    let actions = ask(&mut book, 10, 2, "laya", Priority::Interactive);
    assert_eq!(actions, vec![waiting_until(2, loading("laya"), 60_000)]);

    let actions = book.handle(Moment(900), Event::Loaded { model: m("laya") });
    assert_eq!(
        actions,
        vec![forward(2, "laya"), grant(1, 1), Action::Persist]
    );
}

#[test]
fn a_batch_waiter_waits_out_the_grace_period() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = take(&mut book, 30_000, 1, 1, "iq2_xs", None);
    let reason = grace("qwen3.8:27b", 120_000);
    assert_eq!(actions, vec![waiting_until(1, reason, 180_000)]);
    assert_eq!(book.next_deadline(), Some(Moment(120_000)));
    assert_eq!(tick(&mut book, 119_999), vec![]);

    let actions = tick(&mut book, 120_000);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq2_xs")),
        ]
    );
}

#[test]
fn an_interactive_waiter_does_not_wait_for_grace() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = ask(&mut book, 30_000, 1, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq2_xs")),
        ]
    );
}

#[test]
fn a_batch_request_is_refused_when_grace_and_a_load_pass_its_cap() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = ask(&mut book, 30_000, 1, "iq2_xs", Priority::Batch);
    assert_eq!(
        actions,
        vec![Action::Refuse {
            waiter: WaiterId(1),
            refusal: Refusal {
                reason: grace("qwen3.8:27b", 120_000),
                retry_after: Some(Duration::from_secs(150)),
            },
        }]
    );
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Loaded));
}

#[test]
fn a_busy_model_keeps_resetting_the_grace_period() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = take(&mut book, 30_000, 1, 1, "iq2_xs", None);

    let actions = ask(&mut book, 100_000, 2, "qwen3.8:27b", Priority::Interactive);
    let reason = grace("qwen3.8:27b", 220_000);
    assert_eq!(
        actions,
        vec![forward(2, "qwen3.8:27b"), waiting_until(1, reason, 280_000)]
    );
    let actions = book.handle(Moment(100_000), finished(2, QWEN));
    assert_eq!(actions, vec![]);
    assert_eq!(tick(&mut book, 120_000), vec![]);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Loaded));
    assert_eq!(book.next_deadline(), Some(Moment(220_000)));

    let actions = tick(&mut book, 220_000);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq2_xs")),
        ]
    );
}

#[test]
fn a_model_with_requests_in_flight_is_inside_its_grace_period() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "qwen3.8:27b", Priority::Interactive);
    let actions = book.handle(
        Moment(0),
        Event::Loaded {
            model: m("qwen3.8:27b"),
        },
    );
    assert_eq!(actions, vec![forward(1, "qwen3.8:27b")]);

    // Still in flight at 3 min, so its grace could end 2 min from now at the earliest.
    let actions = take(&mut book, 180_000, 2, 1, "iq2_xs", None);
    let reason = grace("qwen3.8:27b", 300_000);
    assert_eq!(actions, vec![waiting_until(2, reason, 360_000)]);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Loaded));

    let actions = book.handle(Moment(240_000), finished(1, QWEN));
    let reason = grace("qwen3.8:27b", 360_000);
    assert_eq!(actions, vec![waiting_until(2, reason, 420_000)]);
    assert_eq!(book.next_deadline(), Some(Moment(360_000)));
    assert_eq!(tick(&mut book, 359_999), vec![]);

    let actions = tick(&mut book, 360_000);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(2, loading("iq2_xs")),
        ]
    );
}

#[test]
fn a_request_past_its_deadline_is_refused_with_its_reason() {
    let mut book = book();
    let actions = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_000),
        ]
    );
    assert_eq!(tick(&mut book, 119_999), vec![]);

    let actions = tick(&mut book, 120_000);
    let refusal = Refusal {
        reason: loading("laya"),
        retry_after: None,
    };
    assert_eq!(
        actions,
        vec![Action::Refuse {
            waiter: WaiterId(1),
            refusal,
        }]
    );
    assert_eq!(tick(&mut book, 130_000), vec![]);
}

#[test]
fn the_first_load_estimates_sixty_seconds() {
    let mut book = book();
    let actions = ask(&mut book, 1_000, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 61_000),
        ]
    );
}

#[test]
fn later_loads_estimate_from_the_last_one() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let _ = book.handle(Moment(45_000), Event::Loaded { model: m("laya") });
    let _ = book.handle(Moment(45_000), finished(1, "laya"));
    let _ = book.handle(Moment(50_000), Event::BackendExited { model: m("laya") });
    let _ = book.handle(Moment(51_000), Event::Unloaded { model: m("laya") });

    let actions = ask(&mut book, 60_000, 2, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(2, loading("laya"), 105_000),
        ]
    );
}

#[test]
fn an_idle_model_unloads() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    assert_eq!(tick(&mut book, 8 * 3_600_000 - 1), vec![]);

    let actions = tick(&mut book, 8 * 3_600_000);
    assert_eq!(actions, vec![Action::Unload(m("laya"))]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloading));
}

#[test]
fn next_deadline_is_the_earliest_pending_moment() {
    let mut book = book();
    assert_eq!(book.next_deadline(), None);
    warm(&mut book, 0, "laya");
    assert_eq!(book.next_deadline(), Some(Moment(8 * 3_600_000)));

    let _ = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(book.next_deadline(), Some(Moment(120_010)));
}

#[test]
fn the_blocker_is_the_model_in_the_way_not_the_first_by_name() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let _ = ask(&mut book, 10, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Loading));

    let actions = ask(&mut book, 20, 3, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![waiting_until(3, behind("qwen3.8:27b"), 60_010)]
    );
}

#[test]
fn a_batch_waiter_waits_out_grace_rather_than_behind_a_held_model() {
    let mut book = book_from(THREE_EVEN_MODELS);
    let _ = take(&mut book, 0, 1, 1, "a", None);
    let _ = book.handle(Moment(0), Event::Loaded { model: m("a") });
    warm(&mut book, 10_000, "b");

    let actions = take(&mut book, 20_000, 2, 2, "c", None);
    assert_eq!(
        actions,
        vec![waiting_until(2, grace("b", 130_000), 190_000)]
    );

    let actions = tick(&mut book, 130_000);
    assert_eq!(
        actions,
        vec![Action::Unload(m("b")), waiting(2, loading("c"))]
    );
    assert_eq!(book.state(&m("a")), Some(State::Loaded));
}

#[test]
fn a_reserved_model_drops_its_claim_when_its_last_waiter_is_refused() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    assert_eq!(
        ask(&mut book, 0, 1, "iq2_xs", Priority::Interactive),
        [forward(1, "iq2_xs")]
    );
    assert_eq!(
        ask(&mut book, 10, 2, QWEN, Priority::Interactive),
        [waiting(2, loading(QWEN))]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Evicting));

    let actions = tick(&mut book, 120_010);

    assert_eq!(actions, [refuse(2, loading(QWEN))]);
    assert_eq!(book.state(&m(QWEN)), Some(State::Unloaded));
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Evicting));
    assert_eq!(book.slots[&m("iq2_xs")].for_model, None);
}

/// d is behind c's claim while c waits on a's eviction. When c's waiter is refused and c drops
/// its claim, d takes the room a frees in the same event, rather than staying behind a claim that
/// is gone.
#[test]
fn a_waiter_behind_a_claim_that_is_dropped_claims_the_room_at_once() {
    let mut book = book_from(&format!(
        r#"{THREE_EVEN_MODELS}
[models.d]
backend = "ollama"
name = "d"
vram = "10G"
ram = "1G"
idle = "1h"
"#
    ));
    warm(&mut book, 0, "a");
    let _ = take(&mut book, 0, 1, 1, "b", None);
    let _ = book.handle(Moment(0), Event::Loaded { model: m("b") });
    assert_eq!(
        ask(&mut book, 10, 2, "c", Priority::Interactive),
        [Action::Unload(m("a")), waiting(2, loading("c"))]
    );
    assert_eq!(
        ask(&mut book, 20, 3, "d", Priority::Interactive),
        [waiting(3, behind("c"))]
    );

    let actions = tick(&mut book, 120_010);

    assert_eq!(actions, [refuse(2, loading("c")), waiting(3, loading("d"))]);
    assert_eq!(book.state(&m("c")), Some(State::Unloaded));
    assert_eq!(book.state(&m("d")), Some(State::Reserved));
}

#[test]
fn a_bare_lease_waiter_view_carries_its_footprint() {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, "iq2_xs", None);
    let _ = book.handle(Moment(10), Event::Loaded { model: m("iq2_xs") });
    let _ = ask_lease(&mut book, 20, 2, bare(2, Vram::Bytes(8 * GIB), 2));

    let waiters = book.snapshot(Moment(30)).waiters;

    assert_eq!(waiters.len(), 1);
    assert_eq!(waiters[0].kind, WaiterKind::Lease);
    assert_eq!(waiters[0].model, None);
    assert_eq!(
        waiters[0].footprint,
        Some(Footprint {
            vram: Vram::Bytes(8 * GIB),
            ram: 2 * GIB,
        })
    );
}

#[test]
fn a_request_waiter_view_has_no_footprint() {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, "iq2_xs", None);
    let _ = book.handle(Moment(10), Event::Loaded { model: m("iq2_xs") });
    let _ = ask(&mut book, 20, 2, "qwen3.8:27b", Priority::Interactive);
    let _ = take(&mut book, 25, 3, 3, "iq3_s", None);

    let waiters = book.snapshot(Moment(30)).waiters;

    assert!(!waiters.is_empty());
    assert!(waiters.iter().all(|waiter| waiter.footprint.is_none()));
}

/// bench-01's lease 1 holds qwen, and a batch lease for iq2_xs, which needs qwen's room, queues.
fn swap_waits_on_a_held_qwen() -> Book {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, QWEN, None);
    let _ = book.handle(Moment(0), Event::Loaded { model: m(QWEN) });
    let _ = take(&mut book, 1_000, 2, 2, "iq2_xs", None);
    book
}

#[test]
fn a_waiter_needing_a_swap_holds_back_a_newer_lease_on_the_loaded_model() {
    let mut book = swap_waits_on_a_held_qwen();

    let actions = take(&mut book, 2_000, 3, 3, QWEN, None);

    assert_eq!(actions, [waiting(3, behind("iq2_xs"))]);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
}

#[test]
fn the_swap_happens_once_the_held_leases_end() {
    let mut book = book();
    for (waiter, lease) in [(1, 1), (4, 4)] {
        let _ = take(&mut book, 0, waiter, lease, QWEN, None);
    }
    let _ = book.handle(Moment(0), Event::Loaded { model: m(QWEN) });
    let _ = take(&mut book, 1_000, 2, 2, "iq2_xs", None);
    let _ = take(&mut book, 2_000, 3, 3, QWEN, None);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));

    let actions = book.handle(Moment(300_000), Event::LeaseReleased { lease: LeaseId(1) });
    assert_eq!(actions, [ended(1, Ended::Released), Action::Persist]);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));

    let actions = book.handle(Moment(300_001), Event::LeaseReleased { lease: LeaseId(4) });
    assert_eq!(
        actions,
        [
            Action::Unload(m(QWEN)),
            waiting(2, loading("iq2_xs")),
            waiting(
                3,
                Reason::Evicting {
                    model: m(QWEN),
                    for_model: m("iq2_xs").into(),
                }
            ),
            ended(4, Ended::Released),
            Action::Persist,
        ]
    );
}

#[test]
fn an_interactive_lease_ahead_of_the_swap_is_still_admitted() {
    let mut book = swap_waits_on_a_held_qwen();
    let ask = LeaseAsk {
        priority: Priority::Interactive,
        ..lease_ask(3, QWEN)
    };

    let actions = ask_lease(&mut book, 2_000, 3, ask);

    assert_eq!(actions[0], grant(3, 3));
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
}

#[test]
fn a_lease_on_a_model_that_fits_beside_is_not_held_back() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = take(&mut book, 0, 1, 1, QWEN, None);
    let _ = book.handle(Moment(0), Event::Loaded { model: m(QWEN) });
    let _ = take(&mut book, 1_000, 2, 2, "iq2_xs", None);

    let actions = take(&mut book, 2_000, 3, 3, "laya", None);

    assert_eq!(actions[0], grant(3, 3));
}

#[test]
fn a_request_under_no_lease_still_takes_the_request_turn() {
    let mut book = book_from(&turn::turns_toml(1));
    warm(&mut book, 0, "iq3_s");
    let _ = ask_lease(&mut book, 0, 1, turn::noted(1, "#79"));
    // iq3_s excludes laya, so this lease needs iq3_s's room.
    let _ = take(&mut book, 1_000, 2, 2, "laya", None);

    let actions = ask(&mut book, 2_000, 3, "iq3_s", Priority::Batch);

    assert_eq!(actions[0], forward(3, "iq3_s"));
}

#[test]
fn a_held_back_lease_says_which_waiter_it_waits_behind() {
    let mut book = swap_waits_on_a_held_qwen();
    let _ = take(&mut book, 2_000, 3, 3, QWEN, None);

    let waiters = book.snapshot(Moment(3_000)).waiters;

    let held_back = waiters
        .iter()
        .find(|waiter| waiter.model == Some(m(QWEN)))
        .expect("the lease on qwen waits");
    assert_eq!(held_back.reason, Some(behind("iq2_xs")));
}
