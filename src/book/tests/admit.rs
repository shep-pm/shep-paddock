use super::*;

// Three 10G models on a 24G card: any two fit, all three do not, so
// evicting either loaded one makes room for the third.
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

// A 1G model fits beside qwen but not beside a model that takes all the VRAM.
const SMALL_MODEL: &str = r#"
[models.embed]
backend = "ollama"
name = "embed"
vram = "1G"
ram = "1G"
idle = "1h"
"#;

#[test]
fn a_request_for_a_loaded_model_forwards_at_once() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![forward(1, "qwen3.8:27b")]);
}

#[test]
fn a_model_that_fits_loads_then_forwards() {
    let mut book = book();
    let actions = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_000),
        ]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Loading));

    let actions = book.handle(Moment(900), Event::Loaded { model: m("laya") });
    assert_eq!(actions, vec![forward(1, "laya")]);
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
    assert_eq!(
        book.slots[&m("laya")].load_took,
        Some(Duration::from_millis(900))
    );
}

#[test]
fn two_waiters_on_one_model_share_its_load() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let actions = ask(&mut book, 5, 2, "laya", Priority::Interactive);
    assert_eq!(actions, vec![waiting_until(2, loading("laya"), 60_000)]);

    let actions = book.handle(Moment(900), Event::Loaded { model: m("laya") });
    assert_eq!(actions, vec![forward(1, "laya"), forward(2, "laya")]);
}

#[test]
fn strata_evicts_an_idle_qwen_and_loads_after_it_unloads() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq2_xs")),
        ]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Reserved));
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Unloading));

    let actions = book.handle(
        Moment(20),
        Event::Unloaded {
            model: m("qwen3.8:27b"),
        },
    );
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq2_xs")),
            waiting_until(1, loading("iq2_xs"), 60_020),
        ]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loading));
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Unloaded));
}

#[test]
fn eviction_waits_for_in_flight_requests_to_finish() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let actions = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![forward(1, "qwen3.8:27b")]);

    let actions = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);
    assert_eq!(actions, vec![waiting(2, loading("iq2_xs"))]);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Evicting));
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Reserved));

    let qwen = || m("qwen3.8:27b");
    let actions = book.handle(Moment(30), Event::RequestFinished { model: qwen() });
    assert_eq!(actions, vec![Action::Unload(qwen())]);
    assert_eq!(book.state(&qwen()), Some(State::Unloading));

    let actions = book.handle(Moment(40), Event::Unloaded { model: qwen() });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq2_xs")),
            waiting_until(2, loading("iq2_xs"), 60_040),
        ]
    );
}

#[test]
fn an_exclusion_forces_an_eviction_the_numbers_would_allow() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let actions = ask(&mut book, 10, 1, "iq3_s", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, loading("iq3_s"))]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Unloading));
    assert_eq!(book.state(&m("iq3_s")), Some(State::Reserved));
}

#[test]
fn eviction_picks_the_least_recently_used_model() {
    let mut book = book_from(THREE_EVEN_MODELS);
    warm(&mut book, 0, "a");
    warm(&mut book, 10, "b");
    let _ = ask(&mut book, 20, 1, "a", Priority::Interactive);
    let _ = book.handle(Moment(20), Event::RequestFinished { model: m("a") });

    let actions = ask(&mut book, 30, 2, "c", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("b")), waiting(2, loading("c"))]
    );
    assert_eq!(book.state(&m("a")), Some(State::Loaded));
}

#[test]
fn eviction_does_not_take_a_model_the_fit_does_not_need() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    warm(&mut book, 10, "qwen3.8:27b");
    let actions = ask(&mut book, 20, 1, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq2_xs")),
        ]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
}

#[test]
fn a_reserved_model_loads_once_every_eviction_is_done() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    warm(&mut book, 10, "qwen3.8:27b");
    let actions = ask(&mut book, 20, 1, "iq3_s", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("laya")),
            Action::Unload(m("qwen3.8:27b")),
            waiting(1, loading("iq3_s")),
        ]
    );

    let actions = book.handle(Moment(30), Event::Unloaded { model: m("laya") });
    assert_eq!(actions, vec![]);
    assert_eq!(book.state(&m("iq3_s")), Some(State::Reserved));

    let actions = book.handle(
        Moment(40),
        Event::Unloaded {
            model: m("qwen3.8:27b"),
        },
    );
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq3_s")),
            waiting_until(1, loading("iq3_s"), 60_040),
        ]
    );
}

#[test]
fn a_committed_eviction_queues_new_requests_for_the_evicted_model() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    let _ = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);

    let actions = ask(&mut book, 30, 3, "qwen3.8:27b", Priority::Interactive);
    let reason = Reason::Evicting {
        model: m("qwen3.8:27b"),
        for_model: m("iq2_xs"),
    };
    assert_eq!(actions, vec![waiting(3, reason)]);
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Evicting));
}

#[test]
fn a_waiter_whose_model_fits_skips_a_blocked_one() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    let _ = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);
    let actions = ask(&mut book, 30, 3, "iq2_xs-256k", Priority::Interactive);
    assert_eq!(actions, vec![waiting(3, behind("iq2_xs"))]);

    let actions = ask(&mut book, 40, 4, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(4, loading("laya"), 60_040),
        ]
    );
    assert_eq!(book.state(&m("iq2_xs-256k")), Some(State::Unloaded));
}

#[test]
fn a_reserved_model_keeps_its_room_from_a_later_waiter() {
    let mut book = book_from(&format!("{}{SMALL_MODEL}", test_support::HOST_AND_MODELS));
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    let _ = ask(&mut book, 20, 2, "iq2_xs", Priority::Interactive);

    let actions = ask(&mut book, 30, 3, "embed", Priority::Interactive);
    assert_eq!(actions, vec![waiting(3, behind("iq2_xs"))]);
    assert_eq!(book.state(&m("embed")), Some(State::Unloaded));
}

#[test]
fn laya_waits_for_a_reserved_model_that_excludes_it() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = ask(&mut book, 10, 1, "qwen3.8:27b", Priority::Interactive);
    let _ = ask(&mut book, 20, 2, "iq3_s", Priority::Interactive);
    assert_eq!(book.state(&m("iq3_s")), Some(State::Reserved));

    let actions = ask(&mut book, 30, 3, "laya", Priority::Interactive);
    assert_eq!(actions, vec![waiting(3, behind("iq3_s"))]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
}

#[test]
fn interactive_waiters_take_freed_room_before_batch_ones() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "iq2_xs", Priority::Interactive);
    let actions = ask(&mut book, 10, 2, "qwen3.8:27b", Priority::Batch);
    assert_eq!(actions, vec![waiting_until(2, behind("iq2_xs"), 60_000)]);
    let actions = ask(&mut book, 20, 3, "iq2_xs-256k", Priority::Interactive);
    assert_eq!(actions, vec![waiting_until(3, behind("iq2_xs"), 60_000)]);

    let actions = book.handle(Moment(900), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(
        actions,
        vec![
            forward(1, "iq2_xs"),
            waiting(3, loading("iq2_xs-256k")),
            waiting(2, behind("iq2_xs-256k")),
        ]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Evicting));
    assert_eq!(book.state(&m("iq2_xs-256k")), Some(State::Reserved));
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Unloaded));
}

#[test]
fn a_model_just_loaded_serves_its_waiter_before_it_is_evicted() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "iq2_xs", Priority::Batch);
    let actions = ask(&mut book, 10, 2, "qwen3.8:27b", Priority::Interactive);
    assert_eq!(actions, vec![waiting_until(2, behind("iq2_xs"), 60_000)]);

    let actions = book.handle(Moment(900), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(
        actions,
        vec![forward(1, "iq2_xs"), waiting(2, loading("qwen3.8:27b"))]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Evicting));
    assert_eq!(book.state(&m("qwen3.8:27b")), Some(State::Reserved));
}

// On a 24G card, r and r2 can claim room that a and y still hold.
const FIVE_MODELS: &str = r#"
[host]
vram = "24G"
ram = "32G"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.a]
backend = "ollama"
name = "a"
vram = "4G"
ram = "1G"
idle = "1h"

[models.y]
backend = "ollama"
name = "y"
vram = "10G"
ram = "1G"
idle = "1h"

[models.r]
backend = "ollama"
name = "r"
vram = "14G"
ram = "1G"
idle = "1h"

[models.r2]
backend = "ollama"
name = "r2"
vram = "6G"
ram = "1G"
idle = "1h"

[models.x]
backend = "ollama"
name = "x"
vram = "4G"
ram = "1G"
idle = "1h"
"#;

#[test]
fn a_reserved_model_waits_for_memory_still_held() {
    let mut book = book_from(FIVE_MODELS);
    warm(&mut book, 0, "a");
    warm(&mut book, 10, "y");
    let _ = ask(&mut book, 20, 1, "a", Priority::Interactive);
    let _ = ask(&mut book, 30, 2, "y", Priority::Interactive);

    let actions = ask(&mut book, 40, 3, "r", Priority::Interactive);
    assert_eq!(actions, vec![waiting(3, loading("r"))]);
    assert_eq!(book.state(&m("a")), Some(State::Evicting));

    // r2 claims y's room, and loads at once beside the evicting y.
    let actions = ask(&mut book, 50, 4, "r2", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("r2")),
            waiting_until(4, loading("r2"), 60_050),
        ]
    );
    assert_eq!(book.state(&m("y")), Some(State::Evicting));

    let actions = ask(&mut book, 60, 5, "x", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Load(m("x")), waiting_until(5, loading("x"), 60_060),]
    );

    let actions = book.handle(Moment(70), Event::RequestFinished { model: m("a") });
    assert_eq!(actions, vec![Action::Unload(m("a"))]);

    // y still holds 10G, so r's 14G must wait for it.
    let actions = book.handle(Moment(80), Event::Unloaded { model: m("a") });
    assert_eq!(actions, vec![]);
    assert_eq!(book.state(&m("r")), Some(State::Reserved));
    assert_eq!(broken(&book), None);

    let actions = book.handle(Moment(90), Event::RequestFinished { model: m("y") });
    assert_eq!(actions, vec![Action::Unload(m("y"))]);
    let actions = book.handle(Moment(100), Event::Unloaded { model: m("y") });
    assert_eq!(
        actions,
        vec![Action::Load(m("r")), waiting_until(3, loading("r"), 60_100)]
    );
    assert_eq!(broken(&book), None);
}

#[test]
fn a_model_still_unloading_is_waited_for_not_replaced_by_an_eviction() {
    let mut book = book_from(FIVE_MODELS);
    warm(&mut book, 0, "y");
    warm(&mut book, 10, "r2");
    let actions = book.handle(Moment(20), Event::BackendExited { model: m("y") });
    assert_eq!(actions, vec![Action::Unload(m("y"))]);

    let actions = ask(&mut book, 30, 1, "r", Priority::Interactive);
    assert_eq!(actions, vec![waiting(1, loading("r"))]);
    assert_eq!(book.state(&m("r2")), Some(State::Loaded));
    assert_eq!(book.state(&m("r")), Some(State::Reserved));

    let actions = book.handle(Moment(40), Event::Unloaded { model: m("y") });
    assert_eq!(
        actions,
        vec![Action::Load(m("r")), waiting_until(1, loading("r"), 60_040)]
    );
}

#[test]
fn an_excluded_model_still_unloading_delays_the_load() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = book.handle(Moment(10), Event::BackendExited { model: m("laya") });

    let actions = ask(&mut book, 20, 1, "iq3_s", Priority::Interactive);
    assert_eq!(actions, vec![waiting(1, loading("iq3_s"))]);
    assert_eq!(book.state(&m("iq3_s")), Some(State::Reserved));

    let actions = book.handle(Moment(30), Event::Unloaded { model: m("laya") });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("iq3_s")),
            waiting_until(1, loading("iq3_s"), 60_030),
        ]
    );
}

#[test]
fn a_reserved_model_whose_waiters_all_left_drops_its_claim() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    assert_eq!(
        ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive),
        [Action::Unload(m(QWEN)), waiting(1, loading("iq2_xs"))]
    );

    let actions = book.handle(
        Moment(20),
        Event::WaiterGone {
            waiter: WaiterId(1),
        },
    );

    assert_eq!(actions, []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Unloaded));
    assert_eq!(book.state(&m(QWEN)), Some(State::Unloading));
    let qwen = footprint(&book, QWEN);
    assert_eq!(book.snapshot(Moment(20)).declared, qwen);
    assert_eq!(
        ask(&mut book, 30, 2, QWEN, Priority::Interactive),
        [waiting(2, Reason::Draining { model: m(QWEN) })]
    );
    assert_eq!(
        book.handle(Moment(40), Event::Unloaded { model: m(QWEN) }),
        [Action::Load(m(QWEN)), waiting_until(2, loading(QWEN), 40)]
    );
}
