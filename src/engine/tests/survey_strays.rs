//! Strays the survey finds and forgets, and the races `touched` guards. Paused clock, except
//! where ollama is a fake server on a real socket, which needs real time.

use std::collections::BTreeSet;

use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

use super::{
    survey::{LIMIT, asked, surveyed_every, view_of, with_ollama},
    *,
};
use crate::{
    backend::OllamaLoaded,
    engine::survey::{Blobs, Reading},
    footprint::{Footprint, Vram},
    test_support::{
        FakeHost,
        captured::{PS_QWEN, QWEN_BLOB, QWEN_MANIFEST, SHOW_QWEN},
    },
};

const BASE: &str = "http://127.0.0.1:11434";
// Built, not captured: /api/ps with nothing loaded.
const PS_NONE: &str = r#"{"models":[]}"#;

fn surveyed_fast() -> Start {
    surveyed_every(FakeHost::absent(), Duration::from_millis(50))
}

/// One ollama at `base` named `ollama`, serving llama3 alone.
fn llama_only(base: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models.llama3]
backend = "ollama"
name = "llama3:8b"
vram = "6G"
ram = "1G"
idle = "2h"
"#
    ))
}

fn row(sheep: &str, status: ProcStatus) -> ProcessInfo {
    ProcessInfo::builder(1, sheep, status).build()
}

fn listing(asked: Instant) -> Reading {
    let qwen = OllamaLoaded {
        name: "qwen3.8:27b-ctx65536".to_owned(),
        footprint: Footprint {
            vram: Vram::Bytes(17_275_897_773),
            ram: 0,
        },
        digest: Some(QWEN_MANIFEST.to_owned()),
    };
    Reading {
        ollama: vec![(BASE.to_owned(), vec![qwen])],
        ..Reading::empty(asked)
    }
}

fn flock_of(asked: Instant, rows: Vec<ProcessInfo>) -> Reading {
    Reading {
        flock: Some(rows),
        ..Reading::empty(asked)
    }
}

fn idle(_: &str) -> bool {
    false
}

#[tokio::test(start_paused = true)]
async fn a_survey_finds_a_sheep_whose_online_was_missed() {
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");
    let start = surveyed_every(FakeHost::absent(), SURVEY_EVERY);
    with_engine_from(config(SHEEP_MODELS), shepherd, start, |engine| async move {
        until("laya found as a stray", || async {
            view_of(&engine, "laya")
                .await
                .is_some_and(|view| view.state == State::Loaded && view.stray)
        })
        .await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_sheep_stray_the_flock_no_longer_runs_is_forgotten_and_not_stopped() {
    let mut engine = engine();
    let laya = ModelName::from("laya");
    engine.process(online("laya"), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Loaded));
    sleep(SOON).await;

    let lines = engine.surveyed(
        flock_of(Instant::now(), vec![row("laya", ProcStatus::Stopped)]),
        idle,
    );

    assert_eq!(engine.book.state(&laya), Some(State::Unloaded));
    assert!(engine.take_jobs().is_empty(), "nothing is left to stop");
    assert_eq!(
        lines,
        vec!["paddock: laya, a stray, no longer runs; forgetting it".to_owned()]
    );
}

/// The flock was read while laya still ran, and is applied after its exit.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_a_stray_sheep_exits_does_not_bring_it_back() {
    let mut engine = engine();
    let laya = ModelName::from("laya");
    engine.process(online("laya"), idle);
    sleep(SOON).await;
    let asked = Instant::now();
    sleep(SOON).await;
    engine.process(crash("laya", ProcessKind::Exit, false), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Unloaded));

    let _ = engine.surveyed(flock_of(asked, vec![row("laya", ProcStatus::Online)]), idle);
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Unloaded),
        "the reading was older than the exit"
    );

    sleep(SOON).await;
    let lines = engine.surveyed(
        flock_of(Instant::now(), vec![row("laya", ProcStatus::Online)]),
        idle,
    );
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Loaded),
        "a reading taken after it is a stray"
    );
    assert_eq!(
        lines,
        vec!["paddock: sheep laya is running without the dog; counting it as laya".to_owned()]
    );
}

/// `model` loaded by the dog on `config`, serving no request.
fn loaded_by_the_dog(config: Arc<Config>, model: &str) -> Engine {
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(config, Clock::new(), notify);
    let model = ModelName::from(model);
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: model.clone(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    let _ = engine.take_jobs();
    engine.finished(model.clone(), Outcome::Loaded);
    engine.feed(Event::RequestFinished {
        model,
        client: MAC.into(),
    });
    engine
}

/// The survey asked /api/ps while qwen was still loaded, and its answer arrives after the
/// dog unloaded qwen for sitting idle.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_an_unload_does_not_bring_the_model_back() {
    let mut engine = loaded_by_the_dog(with_ollama(BASE), QWEN);
    let qwen = ModelName::from(QWEN);
    let asked = Instant::now();
    sleep(Duration::from_secs(2 * 3_600)).await;
    engine.feed(Event::Tick);
    assert_eq!(engine.book.state(&qwen), Some(State::Unloading));
    let _ = engine.take_jobs();
    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloading),
        "the dog's own unload is not a stray"
    );
    engine.finished(qwen.clone(), Outcome::Unloaded);
    let unloaded_at = Instant::now();

    let _ = engine.surveyed(listing(asked), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloaded),
        "the listing was older than the unload"
    );
    let _ = engine.surveyed(listing(unloaded_at), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloaded),
        "nor is one asked at the unload's instant trusted"
    );

    sleep(SOON).await;
    let lines = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Loaded),
        "a listing taken after it is a stray"
    );
    assert_eq!(
        lines,
        vec![
            "paddock: ollama ollama has qwen3.8:27b-ctx65536 loaded without the dog; \
             counting it as qwen3.8:27b"
                .to_owned()
        ]
    );
}

// Real time: /api/ps comes from a fake server on a real socket.
#[tokio::test]
async fn an_ollama_model_the_dog_did_not_load_is_a_stray_until_it_leaves_api_ps() {
    let (base, _ollama) = fake_http(vec![
        (
            "GET",
            "/api/ps",
            vec![(200, PS_QWEN), (200, PS_QWEN), (200, PS_NONE)],
        ),
        ("POST", "/api/show", vec![(200, SHOW_QWEN)]),
    ]);
    let start = surveyed_fast();
    with_engine_from(
        with_ollama(&base),
        FakeShepherd::new(),
        start,
        |engine| async move {
            until_within(LIMIT, "qwen found as a stray", || async {
                view_of(&engine, QWEN)
                    .await
                    .is_some_and(|view| view.state == State::Loaded && view.stray)
            })
            .await;
            until_within(LIMIT, "qwen forgotten", || async {
                view_of(&engine, QWEN)
                    .await
                    .is_some_and(|view| view.state == State::Unloaded && !view.stray)
            })
            .await;
        },
    )
    .await;
}

// Real time: /api/ps comes from a fake server on a real socket.
#[tokio::test]
async fn an_unknown_ollama_model_is_a_stand_in_at_the_size_ollama_reports() {
    let (base, _ollama) = fake_http(vec![
        ("GET", "/api/ps", vec![(200, PS_QWEN)]),
        ("POST", "/api/show", vec![(200, SHOW_QWEN)]),
    ]);
    let start = surveyed_fast();
    with_engine_from(
        llama_only(&base),
        FakeShepherd::new(),
        start,
        |engine| async move {
            let name = "ollama:qwen3.8:27b-ctx65536";
            until_within(LIMIT, "qwen found as a stand-in", || async {
                view_of(&engine, name).await.is_some()
            })
            .await;
            let view = view_of(&engine, name).await.expect("the stand-in");
            assert_eq!(
                (view.state, view.stray, view.unknown),
                (State::Loaded, true, true)
            );
            assert_eq!(
                view.footprint,
                Footprint {
                    vram: Vram::Bytes(17_275_897_773),
                    ram: 0
                }
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unread_flock_neither_finds_nor_forgets_a_sheep_stray() {
    let mut engine = engine();
    engine.process(online("laya"), idle);
    sleep(SOON).await;

    let lines = engine.surveyed(Reading::empty(Instant::now()), idle);

    assert_eq!(engine.book.state(&"laya".into()), Some(State::Loaded));
    assert!(engine.snapshot().models.iter().any(|view| view.stray));
    assert_eq!(lines, Vec::<String>::new());
}

/// What an ollama that did not answer `/api/ps` reads as: its cached blobs, and its url.
fn unanswered(asked: Instant) -> Reading {
    let at = (BASE.to_owned(), "qwen3.8:27b-ctx65536".to_owned());
    Reading {
        blobs: Blobs::from([(at, (Some(QWEN_MANIFEST.to_owned()), QWEN_BLOB.to_owned()))]),
        unanswered: BTreeSet::from([BASE.to_owned()]),
        ..Reading::empty(asked)
    }
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_ollama_neither_finds_nor_forgets_a_stray() {
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(with_ollama(BASE), Clock::new(), notify);
    let qwen = ModelName::from(QWEN);

    let _ = engine.surveyed(unanswered(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloaded),
        "a cached blob is not a listing"
    );

    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(engine.book.state(&qwen), Some(State::Loaded));
    sleep(SOON).await;
    let lines = engine.surveyed(unanswered(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Loaded),
        "no answer is not an empty list"
    );
    assert_eq!(lines, Vec::<String>::new());
}

// Real time: /api/ps comes from a fake server on a real socket.
#[tokio::test]
async fn an_ollama_that_stops_answering_keeps_its_stray() {
    let (base, ollama) = fake_http(vec![
        ("GET", "/api/ps", vec![(200, PS_QWEN), (500, "")]),
        ("POST", "/api/show", vec![(200, SHOW_QWEN)]),
    ]);
    let start = surveyed_fast();
    with_engine_from(
        with_ollama(&base),
        FakeShepherd::new(),
        start,
        |engine| async move {
            until_within(LIMIT, "three surveys", || async {
                asked(&ollama, "GET", "/api/ps") >= 3
            })
            .await;
            let qwen = view_of(&engine, QWEN).await.expect("qwen");
            assert_eq!((qwen.state, qwen.stray), (State::Loaded, true));
        },
    )
    .await;
}

/// A hand start races the dog's idle stop of iq2_xs, and outlives it.
#[tokio::test(start_paused = true)]
async fn an_online_ignored_while_the_dog_stopped_its_sheep_is_found_by_a_later_survey() {
    let mut engine = loaded_by_the_dog(config(SHEEP_MODELS), "iq2_xs");
    let iq2_xs = ModelName::from("iq2_xs");
    let stand_in = ModelName::from("sheep:iq2_xs");
    sleep(Duration::from_secs(2 * 3_600)).await;
    engine.feed(Event::Tick);
    assert_eq!(engine.book.state(&iq2_xs), Some(State::Unloading));
    let _ = engine.take_jobs();
    engine.process(online("iq2_xs"), |_| true);
    let asked = Instant::now();
    sleep(SOON).await;
    let running = || vec![row("iq2_xs", ProcStatus::Online)];
    let _ = engine.surveyed(flock_of(Instant::now(), running()), |_| true);
    assert_eq!(
        engine.book.state(&stand_in),
        None,
        "the stop is still running"
    );
    engine.finished(iq2_xs.clone(), Outcome::Unloaded);
    assert_eq!(engine.book.state(&iq2_xs), Some(State::Unloaded));

    let _ = engine.surveyed(flock_of(asked, running()), idle);
    assert_eq!(
        engine.book.state(&stand_in),
        None,
        "read before the stop ended"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(flock_of(Instant::now(), running()), idle);
    assert_eq!(engine.book.state(&stand_in), Some(State::Loaded));
}

/// shep held laya stopped after its crash, so the dog's stop of it publishes no `Stop` and
/// its mark stays. The hand start's `online` is not counted, and clears the mark.
#[tokio::test(start_paused = true)]
async fn a_hand_start_a_stale_stop_mark_hid_is_found_by_the_next_survey() {
    let mut engine = loaded_by_the_dog(config(SHEEP_MODELS), "laya");
    let laya = ModelName::from("laya");
    engine.process(crash("laya", ProcessKind::Exit, false), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Unloading));
    let _ = engine.take_jobs();
    engine.finished(laya.clone(), Outcome::Unloaded);
    sleep(SOON).await;

    engine.process(online("laya"), idle);
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Unloaded),
        "the mark hid it"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(
        flock_of(Instant::now(), vec![row("laya", ProcStatus::Online)]),
        idle,
    );
    assert_eq!(engine.book.state(&laya), Some(State::Loaded));
    assert!(engine.snapshot().models.iter().any(|view| view.stray));
}
