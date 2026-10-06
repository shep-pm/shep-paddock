//! A survey whose shepherd or ollama did not answer: what it could not read is unknown, not empty.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use super::{
    survey::{MIB, ask_for_laya, laya_in, laya_reading, with_ollama},
    *,
};
use crate::{
    engine::survey::{self, Blobs, Reading},
    survey::gpu,
    test_support::{
        FakeHost,
        captured::{QWEN_BLOB, QWEN_MANIFEST, QWEN_RUNNER_APP, QWEN_RUNNER_PID, qwen_runner_args},
    },
};

// Not dialled: the bare engine does no I/O.
const OLLAMA: &str = "http://127.0.0.1:9";

#[tokio::test(start_paused = true)]
async fn a_flock_not_described_leaves_unaccounted_absent_while_a_sheep_model_is_tracked() {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    sleep(SOON).await;

    engine.surveyed(Reading {
        flock: None,
        ..laya_reading(Instant::now())
    });

    let snapshot = engine.snapshot();
    assert_eq!(snapshot.unaccounted_vram, None, "laya's tree is unknown");
    assert_eq!(laya_in(&snapshot).measured.vram, None);
}

/// shep answers `Describe` of an empty flock with an error, which is no flock at all.
#[tokio::test(start_paused = true)]
async fn a_flock_not_described_counts_as_empty_with_no_sheep_model_tracked() {
    let mut engine = engine();

    engine.surveyed(Reading {
        flock: None,
        ..laya_reading(Instant::now())
    });

    assert_eq!(engine.snapshot().unaccounted_vram, Some(6_000 * MIB));
}

fn qwen_engine() -> Engine {
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = Engine::new(with_ollama(OLLAMA), Clock::new(), notify);
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: QWEN.into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    let _ = engine.take_jobs();
    engine.finished(QWEN.into(), Outcome::Loaded);
    engine
}

/// qwen's runner holding 19542 of the 19600 MiB in use, as one survey read it.
fn qwen_reading(asked: Instant, unanswered: &[&str]) -> Reading {
    Reading {
        gpu: Some(gpu::reading("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP).expect("readable")),
        cmdlines: BTreeMap::from([(QWEN_RUNNER_PID, qwen_runner_args())]),
        unanswered: unanswered.iter().map(|url| (*url).to_owned()).collect(),
        ..Reading::empty(asked)
    }
}

#[tokio::test(start_paused = true)]
async fn an_ollama_that_did_not_answer_leaves_unaccounted_absent_while_it_holds_a_model() {
    let mut engine = qwen_engine();
    sleep(SOON).await;

    engine.surveyed(qwen_reading(Instant::now(), &[OLLAMA]));

    assert_eq!(
        engine.snapshot().unaccounted_vram,
        None,
        "qwen's runner is unknown"
    );
}

#[tokio::test(start_paused = true)]
async fn an_ollama_that_answered_leaves_unaccounted_known() {
    let mut engine = qwen_engine();
    sleep(SOON).await;

    engine.surveyed(qwen_reading(Instant::now(), &[]));

    assert_eq!(
        engine.snapshot().unaccounted_vram,
        Some(19_600 * MIB),
        "/api/ps listed nothing, so the runner is nobody's"
    );
}

// Real time: ollama is a fake server on a real socket.
#[tokio::test]
async fn a_survey_keeps_the_blobs_of_an_ollama_that_did_not_answer() {
    let (base, _ollama) = fake_http(vec![("GET", "/api/ps", vec![(500, "")])]);
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let at = (base.clone(), "qwen3.8:27b-ctx65536".to_owned());
    let known = Blobs::from([(at, (Some(QWEN_MANIFEST.to_owned()), QWEN_BLOB.to_owned()))]);

    let reading = timeout(
        BOUND,
        survey::read(
            &backends,
            Rc::new(FakeHost::absent()),
            with_ollama(&base),
            known.clone(),
        ),
    )
    .await
    .expect("a reading");

    assert_eq!(reading.blobs, known);
    assert_eq!(reading.unanswered, [base].into());
}

/// qwen's runner as one survey read it, with its arguments unread: `/proc` timed out.
fn qwen_runner_unread(asked: Instant, apps: &str) -> Reading {
    let at = (OLLAMA.to_owned(), "qwen3.8:27b-ctx65536".to_owned());
    Reading {
        gpu: Some(gpu::reading("24600 MiB, 24564 MiB\n", apps).expect("readable")),
        blobs: Blobs::from([(at, (Some(QWEN_MANIFEST.to_owned()), QWEN_BLOB.to_owned()))]),
        unread_cmdlines: BTreeSet::from([QWEN_RUNNER_PID]),
        ..Reading::empty(asked)
    }
}

/// Built, not captured: qwen's runner holding more than 10% over qwen's 22323 MiB.
const QWEN_RUNNER_OVER: &str = "190784, /usr/lib/ollama/llama-server, 24560 MiB\n";

#[tokio::test(start_paused = true)]
async fn a_gpu_process_whose_arguments_went_unread_leaves_unaccounted_absent() {
    let mut engine = qwen_engine();
    sleep(SOON).await;

    let _ = engine.surveyed(qwen_runner_unread(Instant::now(), QWEN_RUNNER_APP));

    let snapshot = engine.snapshot();
    assert_eq!(snapshot.unaccounted_vram, None, "the runner may be qwen's");
    let qwen = snapshot
        .models
        .iter()
        .find(|view| view.name == ModelName::from(QWEN));
    assert_eq!(qwen.expect("qwen").measured.vram, None);
}

#[tokio::test(start_paused = true)]
async fn a_runner_whose_arguments_went_unread_keeps_its_models_drift() {
    let mut engine = qwen_engine();
    sleep(SOON).await;
    let mut all_read = qwen_runner_unread(Instant::now(), QWEN_RUNNER_OVER);
    all_read.unread_cmdlines.clear();
    all_read.cmdlines = BTreeMap::from([(QWEN_RUNNER_PID, qwen_runner_args())]);
    assert_eq!(engine.surveyed(all_read).len(), 1, "qwen starts drifting");

    sleep(SURVEY_EVERY).await;
    let unread = engine.surveyed(qwen_runner_unread(Instant::now(), QWEN_RUNNER_OVER));

    assert_eq!(unread, Vec::<String>::new());
    let snapshot = engine.snapshot();
    let qwen = snapshot
        .models
        .iter()
        .find(|view| view.name == ModelName::from(QWEN));
    assert!(qwen.expect("qwen").drift);
}

#[tokio::test(start_paused = true)]
async fn a_tracked_sheeps_own_process_unread_still_leaves_unaccounted_known() {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    sleep(SOON).await;

    let _ = engine.surveyed(Reading {
        unread_cmdlines: BTreeSet::from([1001]),
        ..laya_reading(Instant::now())
    });

    assert_eq!(
        engine.snapshot().unaccounted_vram,
        Some(2_000 * MIB),
        "1001 is laya's lamb"
    );
}

#[tokio::test(start_paused = true)]
async fn a_survey_tells_arguments_it_could_not_read_from_a_process_that_is_gone() {
    let host = FakeHost::printing(
        "19600 MiB, 24564 MiB\n",
        "190784, /usr/lib/ollama/llama-server, 19542 MiB\n4242, /usr/bin/python3, 58 MiB\n",
    )
    .with_cmdline_unread(QWEN_RUNNER_PID);
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());

    let reading = timeout(
        BOUND,
        survey::read(&backends, Rc::new(host), config(SHEEP_MODELS), Blobs::new()),
    )
    .await
    .expect("a reading");

    assert_eq!(reading.unread_cmdlines, BTreeSet::from([QWEN_RUNNER_PID]));
    assert!(reading.cmdlines.is_empty(), "4242 is gone");
}
