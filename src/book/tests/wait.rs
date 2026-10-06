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
    let qwen = m("qwen3.8:27b");
    let actions = book.handle(Moment(100_000), Event::RequestFinished { model: qwen });
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

    let qwen = m("qwen3.8:27b");
    let actions = book.handle(Moment(240_000), Event::RequestFinished { model: qwen });
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
    let _ = book.handle(Moment(45_000), Event::RequestFinished { model: m("laya") });
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
