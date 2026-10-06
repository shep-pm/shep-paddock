//! Measurement through the survey loop, against a fake shepherd and a fake host, on a paused
//! clock, except where ollama is a fake server on a real socket, which needs real time.

use std::rc::Rc;

use shep_client::shep_core::{
    protocol::{Lamb, ProcessInfo},
    status::ProcStatus,
};

use super::*;
use crate::{
    book::{ModelView, Snapshot},
    engine::survey::{Reading, Survey},
    survey::{Measured, gpu},
    test_support::{
        FakeHost, FakeHttp,
        captured::{PS_QWEN, QWEN_RUNNER_APP, QWEN_RUNNER_PID, SHOW_QWEN, qwen_runner_args},
    },
};

pub(super) const MIB: u64 = 1 << 20;
// Past a few surveys at 50 ms and a few ollama answers on loopback.
pub(super) const LIMIT: Duration = Duration::from_secs(10);

pub(super) fn surveyed_every(host: FakeHost, every: Duration) -> Start {
    Start {
        survey: Some(Survey {
            host: Rc::new(host),
            every,
        }),
        ..Start::default()
    }
}

/// qwen on one ollama at `base` named `gpu-ollama`, under the name the capture lists.
pub(super) fn with_ollama(base: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.gpu-ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "gpu-ollama"
name = "qwen3.8:27b-ctx65536"
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ))
}

pub(super) async fn view_of(engine: &EngineHandle, model: &str) -> Option<ModelView> {
    let model = ModelName::from(model);
    engine
        .snapshot()
        .await
        .models
        .into_iter()
        .find(|view| view.name == model)
}

pub(super) fn asked(server: &FakeHttp, method: &str, path: &str) -> usize {
    server
        .seen()
        .iter()
        .filter(|seen| seen.method == method && seen.path == path)
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_survey_measures_a_sheep_model_and_reports_unaccounted_memory() {
    let shepherd = FakeShepherd::new();
    let host = FakeHost::printing(
        "6000 MiB, 24564 MiB\n",
        "1001, /usr/bin/python3, 4000 MiB\n4242, /usr/bin/python3, 1500 MiB\n",
    );
    let start = surveyed_every(host, SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            shepherd.set_lambs("laya", &[(1001, "python3")]);
            shepherd.set_memory("laya", 1_504 * MIB);

            until("a survey", || async {
                engine.snapshot().await.unaccounted_vram.is_some()
            })
            .await;
            let laya = view_of(&engine, "laya").await.expect("laya");
            assert_eq!(
                laya.measured,
                Measured {
                    vram: Some(4_000 * MIB),
                    ram: Some(1_504 * MIB)
                }
            );
            assert!(laya.drift, "laya declares no VRAM and holds 4000 MiB");
            assert_eq!(engine.snapshot().await.unaccounted_vram, Some(2_000 * MIB));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn unaccounted_is_absent_while_a_model_declared_all_is_loaded() {
    let shepherd = FakeShepherd::new();
    let host = FakeHost::printing(
        "23800 MiB, 24564 MiB\n",
        crate::test_support::captured::STRATA_ENGINE,
    );
    let start = surveyed_every(host, SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            shepherd.set_memory("iq2_xs", 80 * MIB);

            until("a survey", || async {
                view_of(&engine, "iq2_xs")
                    .await
                    .is_some_and(|view| view.measured.ram.is_some())
            })
            .await;
            assert_eq!(
                view_of(&engine, "iq2_xs")
                    .await
                    .expect("iq2_xs")
                    .measured
                    .vram,
                None
            );
            assert_eq!(engine.snapshot().await.unaccounted_vram, None);
        },
    )
    .await;
}

/// Waits for a survey to measure laya's RAM, then checks nothing on the GPU was measured.
async fn laya_measured_without_the_gpu(host: FakeHost) {
    let shepherd = FakeShepherd::new();
    let start = surveyed_every(host, SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            shepherd.set_memory("laya", 1_504 * MIB);

            until("a survey", || async {
                view_of(&engine, "laya")
                    .await
                    .is_some_and(|view| view.measured.ram.is_some())
            })
            .await;
            let laya = view_of(&engine, "laya").await.expect("laya");
            assert_eq!((laya.measured.vram, laya.drift), (None, false));
            assert_eq!(
                engine.snapshot().await.unaccounted_vram,
                None,
                "absent, not zero"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn without_nvidia_smi_the_gpu_figures_are_absent() {
    laya_measured_without_the_gpu(FakeHost::absent()).await;
}

#[tokio::test(start_paused = true)]
async fn unreadable_nvidia_smi_output_counts_as_absent() {
    laya_measured_without_the_gpu(FakeHost::printing("garbage\n", "")).await;
}

/// Built, not captured: `nvidia-smi` prints `[N/A]` for a figure it cannot read.
#[tokio::test(start_paused = true)]
async fn totals_nvidia_smi_cannot_read_count_as_absent() {
    let apps = "1001, /usr/bin/python3, 4000 MiB\n";
    laya_measured_without_the_gpu(FakeHost::printing("[N/A], [N/A]\n", apps)).await;
}

// Real time: ollama is a fake server on a real socket.
#[tokio::test]
async fn an_ollama_model_is_measured_by_the_runner_that_loads_its_blob() {
    let (base, ollama) = fake_http(vec![
        ("POST", "/api/generate", vec![(200, "{}")]),
        ("GET", "/api/ps", vec![(200, PS_QWEN)]),
        ("POST", "/api/show", vec![(200, SHOW_QWEN)]),
    ]);
    let host = FakeHost::printing("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP)
        .with_cmdline(QWEN_RUNNER_PID, qwen_runner_args());
    let start = surveyed_every(host, Duration::from_millis(50));
    with_engine_from(
        with_ollama(&base),
        FakeShepherd::new(),
        start,
        |engine| async move {
            drop(forwarded(&engine, QWEN).await);

            until_within(LIMIT, "qwen measured", || async {
                view_of(&engine, QWEN)
                    .await
                    .is_some_and(|view| view.measured.vram.is_some())
            })
            .await;
            let qwen = view_of(&engine, QWEN).await.expect("qwen");
            assert_eq!(
                (qwen.measured.vram, qwen.drift),
                (Some(19_542 * MIB), false)
            );
            assert_eq!(engine.snapshot().await.unaccounted_vram, Some(58 * MIB));

            until_within(LIMIT, "three more surveys", || async {
                asked(&ollama, "GET", "/api/ps") >= 4
            })
            .await;
            assert_eq!(
                asked(&ollama, "POST", "/api/show"),
                1,
                "the blob is asked once while the manifest stays"
            );
        },
    )
    .await;
}

/// Built, not captured: [`PS_QWEN`] after a pull, with a new manifest digest.
const PS_QWEN_PULLED: &str = r#"{"models":[{"name":"qwen3.8:27b-ctx65536","model":"qwen3.8:27b-ctx65536","digest":"0000000000000000000000000000000000000000000000000000000000000001","size":17275897773,"size_vram":17275897773}]}"#;

/// Surveys every 50 ms until ollama has answered `/api/ps` `surveys` times, and returns how
/// often `/api/show` was asked meanwhile.
async fn shows_over(ps: Vec<(u16, &'static str)>, surveys: usize) -> usize {
    let (base, ollama) = fake_http(vec![
        ("GET", "/api/ps", ps),
        ("POST", "/api/show", vec![(200, SHOW_QWEN)]),
    ]);
    let start = surveyed_every(FakeHost::absent(), Duration::from_millis(50));
    let ollama = Rc::new(ollama);
    let seen = Rc::clone(&ollama);
    with_engine_from(
        with_ollama(&base),
        FakeShepherd::new(),
        start,
        |engine| async move {
            // The engine stops once its last handle drops.
            let _engine = engine;
            until_within(LIMIT, "the surveys", || async {
                asked(&seen, "GET", "/api/ps") > surveys
            })
            .await;
        },
    )
    .await;
    asked(&ollama, "POST", "/api/show")
}

// Real time: ollama is a fake server on a real socket.
#[tokio::test]
async fn a_pull_asks_for_the_blob_again() {
    let ps = vec![(200, PS_QWEN), (200, PS_QWEN), (200, PS_QWEN_PULLED)];
    assert_eq!(
        shows_over(ps, 3).await,
        2,
        "once, then once for the new manifest"
    );
}

// Real time: ollama is a fake server on a real socket.
#[tokio::test]
async fn a_model_ollama_stops_listing_leaves_the_blob_cache() {
    let ps = vec![(200, PS_QWEN), (200, r#"{"models":[]}"#), (200, PS_QWEN)];
    assert_eq!(
        shows_over(ps, 3).await,
        2,
        "once, then once when it is listed again"
    );
}

/// Built, not captured: laya's sheep and its python lamb, which holds 4000 of the 6000 MiB in use.
pub(super) fn laya_reading(asked: Instant) -> Reading {
    let gpu = gpu::reading(
        "6000 MiB, 24564 MiB\n",
        "1001, /usr/bin/python3, 4000 MiB\n4242, /usr/bin/python3, 2000 MiB\n",
    )
    .expect("readable");
    let laya = ProcessInfo::builder(1, "laya", ProcStatus::Online)
        .pid(Some(7))
        .lambs(Some(vec![Lamb::new(1001, "python3")]))
        .memory_bytes(Some(1_504 * MIB))
        .build();
    Reading {
        flock: Some(vec![laya]),
        gpu: Some(gpu),
        ..Reading::empty(asked)
    }
}

pub(super) fn laya_in(snapshot: &Snapshot) -> &ModelView {
    let laya = ModelName::from("laya");
    snapshot
        .models
        .iter()
        .find(|view| view.name == laya)
        .expect("laya")
}

pub(super) fn ask_for_laya(engine: &mut Engine, waiter: u64) {
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(waiter),
        client: MAC.into(),
        model: "laya".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
}

#[tokio::test(start_paused = true)]
async fn a_survey_from_before_a_load_finished_does_not_measure_the_load() {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    let before = Instant::now();
    sleep(SOON).await;
    engine.finished("laya".into(), Outcome::Loaded);

    let _ = engine.surveyed(laya_reading(before), |_| false);

    let snapshot = engine.snapshot();
    let laya = laya_in(&snapshot);
    assert_eq!((laya.measured, laya.drift), (Measured::default(), false));
    assert_eq!(
        snapshot.unaccounted_vram,
        Some(2_000 * MIB),
        "what laya's tree held is still laya's"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(laya_reading(Instant::now()), |_| false);
    let snapshot = engine.snapshot();
    assert_eq!(laya_in(&snapshot).measured.vram, Some(4_000 * MIB));
    assert!(laya_in(&snapshot).drift);
}

#[tokio::test(start_paused = true)]
async fn a_survey_from_before_an_unload_finished_counts_nothing_unaccounted() {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    engine.feed(Event::RequestFinished {
        model: "laya".into(),
        client: MAC.into(),
    });
    sleep(Duration::from_secs(9 * 3600)).await;
    engine.feed(Event::Tick);
    assert!(
        matches!(engine.take_jobs().as_slice(), [Job::Unload(model)] if model.name == ModelName::from("laya"))
    );
    let before = Instant::now();
    sleep(SOON).await;
    engine.finished("laya".into(), Outcome::Unloaded);
    assert_eq!(engine.book.state(&"laya".into()), Some(State::Unloaded));

    let _ = engine.surveyed(laya_reading(before), |_| false);

    let snapshot = engine.snapshot();
    assert_eq!(laya_in(&snapshot).measured, Measured::default());
    assert_eq!(
        snapshot.unaccounted_vram,
        Some(2_000 * MIB),
        "laya held its 4000 MiB when the survey was read"
    );
}

#[tokio::test(start_paused = true)]
async fn a_load_after_the_last_survey_hides_what_that_survey_measured() {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    sleep(SOON).await;
    let _ = engine.surveyed(laya_reading(Instant::now()), |_| false);
    assert_eq!(laya_in(&engine.snapshot()).measured.vram, Some(4_000 * MIB));

    sleep(SOON).await;
    engine.feed(Event::BackendExited {
        model: "laya".into(),
    });
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Unloaded);
    ask_for_laya(&mut engine, 2);
    assert!(
        matches!(engine.take_jobs().as_slice(), [Job::Load(model)] if model.name == ModelName::from("laya"))
    );
    engine.finished("laya".into(), Outcome::Loaded);

    let snapshot = engine.snapshot();
    assert_eq!(laya_in(&snapshot).state, State::Loaded);
    assert_eq!(
        (laya_in(&snapshot).measured, laya_in(&snapshot).drift),
        (Measured::default(), false),
        "the figures were the last load's"
    );
}
