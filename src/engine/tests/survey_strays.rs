//! Strays the survey finds and forgets, and the sources it could not read. Paused clock, except
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

pub(super) const BASE: &str = "http://127.0.0.1:11434";
// Built, not captured: /api/ps with nothing loaded.
const PS_NONE: &str = r#"{"models":[]}"#;

fn surveyed_fast() -> Start {
    surveyed_every(FakeHost::absent(), Duration::from_millis(50))
}

/// One ollama at `base` named `gpu-ollama`, serving llama3 alone.
fn llama_only(base: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.gpu-ollama]
kind = "ollama"
url = "{base}"

[models.llama3]
backend = "gpu-ollama"
name = "llama3:8b"
vram = "6G"
ram = "1G"
idle = "2h"
"#
    ))
}

pub(super) fn row(sheep: &str, status: ProcStatus) -> ProcessInfo {
    ProcessInfo::builder(1, sheep, status).build()
}

pub(super) fn listing(asked: Instant) -> Reading {
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

pub(super) fn flock_of(asked: Instant, rows: Vec<ProcessInfo>) -> Reading {
    Reading {
        flock: Some(rows),
        ..Reading::empty(asked)
    }
}

pub(super) fn idle(_: &str) -> bool {
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

/// `model` loaded by the dog on `config`, serving no request.
pub(super) fn loaded_by_the_dog(config: Arc<Config>, model: &str) -> Engine {
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

pub(super) fn listed_nothing(asked: Instant) -> Reading {
    Reading {
        ollama: vec![(BASE.to_owned(), Vec::new())],
        ..Reading::empty(asked)
    }
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
            let name = "gpu-ollama:qwen3.8:27b-ctx65536";
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

/// The spec's sheep models, and qwen on an ollama the config names `gpu-ollama`.
fn sheep_and_ollama() -> Arc<Config> {
    config(&format!(
        r#"{SHEEP_MODELS}
[backends.gpu-ollama]
kind = "ollama"
url = "{BASE}"

[models."qwen3.8:27b"]
backend = "gpu-ollama"
name = "qwen3.8:27b-ctx65536"
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ))
}

#[tokio::test(start_paused = true)]
async fn one_reading_counts_the_strays_of_both_sources() {
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(sheep_and_ollama(), Clock::new(), notify);
    let llama = OllamaLoaded {
        name: "llama3:8b".to_owned(),
        footprint: Footprint {
            vram: Vram::Bytes(5_000_000_000),
            ram: 0,
        },
        digest: None,
    };
    let mut reading = listing(Instant::now());
    reading.ollama[0].1.push(llama);
    reading.flock = Some(vec![
        row("laya", ProcStatus::Online),
        row("iq2_xs", ProcStatus::Online),
        row("postgres", ProcStatus::Online),
    ]);

    let lines = engine.surveyed(reading, idle);

    let strays: Vec<(String, State, bool)> = engine
        .snapshot()
        .models
        .into_iter()
        .filter(|view| view.stray)
        .map(|view| (view.name.to_string(), view.state, view.unknown))
        .collect();
    assert_eq!(
        strays,
        vec![
            ("gpu-ollama:llama3:8b".to_owned(), State::Loaded, true),
            ("laya".to_owned(), State::Loaded, false),
            ("qwen3.8:27b".to_owned(), State::Loaded, false),
            ("sheep:iq2_xs".to_owned(), State::Loaded, true),
        ]
    );
    assert_eq!(
        lines,
        vec![
            "paddock: sheep laya is running without the dog; counting it as laya",
            "paddock: sheep iq2_xs is running without the dog; counting it as sheep:iq2_xs",
            "paddock: ollama gpu-ollama has qwen3.8:27b-ctx65536 loaded without the dog; \
             counting it as qwen3.8:27b",
            "paddock: ollama gpu-ollama has llama3:8b loaded without the dog; \
             counting it as gpu-ollama:llama3:8b",
        ]
    );
}
