use super::*;

fn fail(waiter: u64, error: &str) -> Action {
    Action::Fail {
        waiter: WaiterId(waiter),
        error: error.to_owned(),
    }
}

fn load_failed(book: &mut Book, now: u64, model: &str, error: &str) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::LoadFailed {
            model: m(model),
            error: error.to_owned(),
        },
    )
}

#[test]
fn a_failed_load_is_retried_once_then_fails_its_waiters() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let _ = ask(&mut book, 5, 2, "laya", Priority::Interactive);

    let actions = load_failed(&mut book, 10, "laya", "first");
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_010),
            waiting_until(2, loading("laya"), 60_010),
        ]
    );
    assert_eq!(book.state(&m("laya")), Some(State::Loading));

    let actions = load_failed(&mut book, 20, "laya", "second");
    assert_eq!(actions, vec![fail(1, "second"), fail(2, "second")]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
    assert_eq!(
        book.errors,
        [LoadError {
            model: m("laya"),
            at: Moment(20),
            error: "second".to_owned(),
        }]
    );
}

#[test]
fn a_retried_load_times_itself_from_the_retry() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let _ = load_failed(&mut book, 100, "laya", "first");
    let _ = book.handle(Moment(400), Event::Loaded { model: m("laya") });
    assert_eq!(
        book.slots[&m("laya")].load_took,
        Some(Duration::from_millis(300))
    );
}

#[test]
fn the_error_list_keeps_the_last_twenty() {
    let mut book = book();
    for i in 0..21 {
        let _ = ask(&mut book, i, i, "laya", Priority::Interactive);
        let _ = load_failed(&mut book, i, "laya", "first");
        let _ = load_failed(&mut book, i, "laya", &format!("failure {i}"));
    }
    assert_eq!(book.errors.len(), 20);
    assert_eq!(
        book.errors.front().map(|e| e.error.as_str()),
        Some("failure 1")
    );
}

#[test]
fn a_backend_that_exits_while_loaded_is_stopped() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let actions = book.handle(Moment(10), Event::BackendExited { model: m("laya") });
    assert_eq!(actions, vec![Action::Unload(m("laya"))]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloading));

    let actions = book.handle(Moment(20), Event::Unloaded { model: m("laya") });
    assert_eq!(actions, vec![]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
}

#[test]
fn a_backend_that_exits_while_loading_counts_as_a_failed_load() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    let exited = || Event::BackendExited { model: m("laya") };

    let actions = book.handle(Moment(10), exited());
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_010),
        ]
    );

    let actions = book.handle(Moment(20), exited());
    assert_eq!(actions, vec![fail(1, "backend exited while loading")]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
}

#[test]
fn a_backend_that_exits_while_unloading_changes_nothing() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = book.handle(Moment(10), Event::BackendExited { model: m("laya") });
    let actions = book.handle(Moment(20), Event::BackendExited { model: m("laya") });
    assert_eq!(actions, vec![]);
    assert_eq!(book.state(&m("laya")), Some(State::Unloading));
}

#[test]
fn a_request_for_an_unloading_model_waits_and_reloads_it() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = book.handle(Moment(10), Event::BackendExited { model: m("laya") });

    let actions = ask(&mut book, 20, 1, "laya", Priority::Interactive);
    let draining = Reason::Draining { model: m("laya") };
    assert_eq!(actions, vec![waiting(1, draining)]);

    let actions = book.handle(Moment(30), Event::Unloaded { model: m("laya") });
    // laya's last load, in `warm`, took no time at all.
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 30)
        ]
    );
}

#[test]
fn the_same_reason_is_not_emitted_twice() {
    let mut book = book();
    let actions = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    assert!(actions.contains(&waiting_until(1, loading("laya"), 60_000)));
    let actions = book.handle(Moment(10), Event::Tick);
    assert_eq!(actions, vec![]);
}

#[test]
fn a_waiter_that_leaves_is_forgotten() {
    let mut book = book();
    warm(&mut book, 0, "qwen3.8:27b");
    let _ = ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Reserved));

    let actions = book.handle(
        Moment(20),
        Event::WaiterGone {
            waiter: WaiterId(1),
        },
    );
    assert_eq!(actions, vec![]);

    let actions = book.handle(
        Moment(30),
        Event::Unloaded {
            model: m("qwen3.8:27b"),
        },
    );
    assert_eq!(actions, vec![Action::Load(m("iq2_xs"))]);

    let actions = book.handle(Moment(40), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(actions, vec![]);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
}

#[test]
fn a_request_for_an_unknown_model_fails() {
    let mut book = book();
    let actions = ask(&mut book, 0, 1, "nope", Priority::Interactive);
    assert_eq!(actions, vec![fail(1, "no model named nope")]);
}

#[test]
fn models_on_one_sheep_never_load_together() {
    let laya_b = r#"
[models.laya-b]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;
    let mut book = book_from(&format!("{}{laya_b}", test_support::HOST_AND_MODELS));
    warm(&mut book, 0, "laya");

    let actions = ask(&mut book, 1_000, 1, "laya-b", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, loading("laya-b"))]
    );
    let actions = book.handle(Moment(2_000), Event::Unloaded { model: m("laya") });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya-b")),
            waiting_until(1, loading("laya-b"), 62_000),
        ]
    );
}
