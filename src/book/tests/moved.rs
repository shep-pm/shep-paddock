use super::*;

// laya on its own sheep and tagger on another, both small enough to fit together.
const TWO_SHEEP: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"

[models.tagger]
backend = { sheep = "tagger" }
url = "http://127.0.0.1:8001"
ram = "2G"
idle = "8h"
"#;

fn swap(toml: &str, laya_on: &str, tagger_on: &str) -> String {
    toml.replace(
        "{ sheep = \"laya\" }",
        &format!("{{ sheep = \"{laya_on}\" }}"),
    )
    .replace(
        "{ sheep = \"tagger\" }",
        &format!("{{ sheep = \"{tagger_on}\" }}"),
    )
}

fn laya_failed(error: &str) -> Event {
    Event::LoadFailed {
        model: m("laya"),
        error: error.to_owned(),
    }
}

/// laya keeps running on the sheep laya after the reload points it at laya2 and puts tagger
/// on the sheep laya, so tagger cannot start there until laya is gone.
#[test]
fn a_model_on_the_sheep_a_loaded_model_left_waits_for_it() {
    let mut book = book_from(TWO_SHEEP);
    warm(&mut book, 0, "laya");
    let moved = test_support::config(&swap(TWO_SHEEP, "laya2", "laya"));
    let _ = book.reconfigure(Moment(5), moved);

    let actions = ask(&mut book, 10, 1, "tagger", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, loading("tagger"))]
    );
    let actions = book.handle(Moment(20), Event::Unloaded { model: m("laya") });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("tagger")),
            waiting_until(1, loading("tagger"), 60_020)
        ]
    );
}

/// laya is loading on the sheep laya when a reload moves it onto tagger's sheep. Its retry
/// would restart the sheep tagger runs on, so it waits for the gate like any load.
#[test]
fn a_failed_loads_retry_goes_through_the_gate() {
    let mut book = book_from(&swap(TWO_SHEEP, "laya", "laya2"));
    warm(&mut book, 0, "tagger");
    let _ = ask(&mut book, 10, 1, "laya", Priority::Interactive);
    assert_eq!(book.state(&m("laya")), Some(State::Loading));
    let moved = test_support::config(&swap(TWO_SHEEP, "laya2", "laya2"));
    let _ = book.reconfigure(Moment(20), moved);

    let actions = book.handle(Moment(30), laya_failed("first"));
    assert_eq!(
        actions,
        vec![Action::Unload(m("tagger")), waiting(1, loading("laya"))]
    );

    let actions = book.handle(Moment(40), Event::Unloaded { model: m("tagger") });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_040)
        ]
    );
    let actions = book.handle(Moment(50), laya_failed("second"));
    assert_eq!(
        actions,
        vec![fail(1, "second")],
        "the retry was the one owed"
    );
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
}

/// The first load fails while tagger blocks the retry, and the only waiter leaves. A load asked
/// later gets a retry of its own.
#[test]
fn an_owed_retry_is_dropped_once_nothing_wants_the_model() {
    let mut book = book_from(&swap(TWO_SHEEP, "laya", "laya2"));
    warm(&mut book, 0, "tagger");
    let _ = ask(&mut book, 10, 1, "laya", Priority::Interactive);
    let moved = test_support::config(&swap(TWO_SHEEP, "laya2", "laya2"));
    let _ = book.reconfigure(Moment(20), moved);
    let _ = book.handle(Moment(30), laya_failed("first"));
    let gone = Event::WaiterGone {
        waiter: WaiterId(1),
    };
    let _ = book.handle(Moment(35), gone);
    let _ = book.handle(Moment(40), Event::Unloaded { model: m("tagger") });
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));

    assert_eq!(
        ask(&mut book, 50, 2, "laya", Priority::Interactive)[0],
        Action::Load(m("laya"))
    );
    let actions = book.handle(Moment(60), laya_failed("again"));
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(2, loading("laya"), 60_060)
        ],
        "a fresh load's first failure is retried"
    );
}

/// laya is held, then removed from the config while tagger moves onto its sheep. laya keeps
/// running there, so tagger is refused for its lease.
#[test]
fn a_loaded_model_gone_from_the_config_keeps_its_sheep_excluded() {
    let mut book = book_from(TWO_SHEEP);
    warm(&mut book, 0, "laya");
    assert_eq!(
        ask_lease(&mut book, 0, 1, lease_ask(1, "laya")),
        vec![grant(1, 1), Action::Persist]
    );
    let without_laya = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.tagger]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8001"
ram = "2G"
idle = "8h"
"#;
    let _ = book.reconfigure(Moment(10), test_support::config(without_laya));

    let actions = ask(&mut book, 20, 2, "tagger", Priority::Interactive);
    assert_eq!(actions, vec![refuse(2, held_by_bench("laya", 1, 0))]);
    assert_eq!(book.state(&m("tagger")), Some(State::Unloaded));
}

#[test]
fn a_load_that_succeeds_on_its_retry_owes_the_next_load_a_retry() {
    let mut book = book_from(TWO_SHEEP);
    assert_eq!(
        ask_lease(&mut book, 0, 1, lease_ask(1, "laya")),
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_000)
        ]
    );
    let _ = book.handle(Moment(10), laya_failed("first"));
    let _ = book.handle(Moment(20), Event::Loaded { model: m("laya") });
    let _ = book.handle(Moment(30), Event::BackendExited { model: m("laya") });
    assert_eq!(
        book.handle(Moment(40), Event::Unloaded { model: m("laya") }),
        vec![Action::Load(m("laya"))]
    );
    assert_eq!(
        book.handle(Moment(50), laya_failed("after the crash")),
        vec![Action::Load(m("laya"))]
    );
}

/// tagger is held, so laya's waiter is refused at the failure and nothing wants laya after.
#[test]
fn an_owed_retry_is_dropped_when_its_waiter_is_refused() {
    let mut book = book_from(&swap(TWO_SHEEP, "laya", "laya2"));
    warm(&mut book, 0, "tagger");
    let _ = ask_lease(&mut book, 0, 1, lease_ask(1, "tagger"));
    let _ = ask(&mut book, 10, 2, "laya", Priority::Interactive);
    let moved = test_support::config(&swap(TWO_SHEEP, "laya2", "laya2"));
    let _ = book.reconfigure(Moment(20), moved);
    assert_eq!(
        book.handle(Moment(30), laya_failed("first")),
        vec![refuse(2, held_by_bench("tagger", 1, 0))]
    );
    let _ = book.handle(Moment(40), Event::LeaseReleased { lease: LeaseId(1) });

    let _ = ask(&mut book, 50, 3, "laya", Priority::Interactive);
    let _ = book.handle(Moment(60), Event::Unloaded { model: m("tagger") });
    assert_eq!(book.state(&m("laya")), Some(State::Loading));
    assert_eq!(
        book.handle(Moment(70), laya_failed("again")),
        vec![
            Action::Load(m("laya")),
            waiting_until(3, loading("laya"), 60_070)
        ]
    );
}

/// laya claims room on laya2 while big leaves. Its old sheep laya is free for tagger.
#[test]
fn a_claim_by_a_moved_model_leaves_its_old_sheep_free() {
    let three = format!(
        "{}\n[models.big]\nbackend = {{ sheep = \"big\" }}\nurl = \"http://127.0.0.1:8002\"\nram = \"4G\"\nidle = \"8h\"\n",
        TWO_SHEEP.replace("63439M", "8G")
    );
    let mut book = book_from(&three);
    warm(&mut book, 0, "laya");
    let _ = book.handle(Moment(1), Event::BackendExited { model: m("laya") });
    let _ = book.handle(Moment(2), Event::Unloaded { model: m("laya") });
    warm(&mut book, 3, "big");
    let _ = book.reconfigure(
        Moment(5),
        test_support::config(&swap(&three, "laya2", "laya")),
    );
    assert_eq!(
        ask(&mut book, 10, 1, "laya", Priority::Interactive),
        vec![Action::Unload(m("big")), waiting(1, loading("laya"))]
    );

    assert_eq!(
        ask(&mut book, 11, 2, "tagger", Priority::Interactive),
        vec![
            Action::Load(m("tagger")),
            waiting_until(2, loading("tagger"), 60_011)
        ]
    );
}
