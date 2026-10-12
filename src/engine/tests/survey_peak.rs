//! The peak of what the surveys measured for a model, since its last load, on a paused clock.

use shep_client::shep_core::{
    protocol::{Lamb, ProcessInfo},
    status::ProcStatus,
};

use super::{
    survey::{MIB, ask_for_laya, laya_in},
    *,
};
use crate::{
    engine::survey::Reading,
    survey::{Measured, gpu},
};

fn laya_loaded() -> Engine {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    engine
}

/// Built, not captured: laya's lamb holds `vram` MiB of the GPU, and its sheep's tree `ram` MiB.
fn laya_holding(asked: Instant, vram: u64, ram: u64) -> Reading {
    let gpu = gpu::reading(
        &format!("{vram} MiB, 24564 MiB\n"),
        &format!("1001, /usr/bin/python3, {vram} MiB\n"),
    )
    .expect("readable");
    let laya = ProcessInfo::builder(1, "laya", ProcStatus::Online)
        .pid(Some(7))
        .lambs(Some(vec![Lamb::new(1001, "python3")]))
        .memory_bytes(Some(ram * MIB))
        .build();
    Reading {
        flock: Some(vec![laya]),
        gpu: Some(gpu),
        ..Reading::empty(asked)
    }
}

/// Surveys `engine` with laya holding `vram` and `ram` MiB, a moment after the last survey.
async fn survey_laya(engine: &mut Engine, vram: u64, ram: u64) {
    sleep(SOON).await;
    let _ = engine.surveyed(laya_holding(Instant::now(), vram, ram), |_| false);
}

fn peak(engine: &Engine) -> Measured {
    laya_in(&engine.snapshot()).measured_peak
}

fn mib(vram: Option<u64>, ram: Option<u64>) -> Measured {
    Measured {
        vram: vram.map(|vram| vram * MIB),
        ram: ram.map(|ram| ram * MIB),
    }
}

#[tokio::test(start_paused = true)]
async fn the_peak_is_the_highest_ram_and_vram_read_since_the_load() {
    let mut engine = laya_loaded();

    survey_laya(&mut engine, 3_000, 1_000).await;
    assert_eq!(peak(&engine), mib(Some(3_000), Some(1_000)));

    survey_laya(&mut engine, 4_000, 1_500).await;
    assert_eq!(peak(&engine), mib(Some(4_000), Some(1_500)));
}

#[tokio::test(start_paused = true)]
async fn a_lower_reading_leaves_the_peak() {
    let mut engine = laya_loaded();
    survey_laya(&mut engine, 4_000, 1_500).await;

    survey_laya(&mut engine, 2_000, 800).await;

    let snapshot = engine.snapshot();
    assert_eq!(
        laya_in(&snapshot).measured,
        mib(Some(2_000), Some(800)),
        "the measured figures follow the latest survey"
    );
    assert_eq!(peak(&engine), mib(Some(4_000), Some(1_500)));
}

#[tokio::test(start_paused = true)]
async fn each_figure_keeps_its_own_peak() {
    let mut engine = laya_loaded();
    survey_laya(&mut engine, 4_000, 800).await;

    survey_laya(&mut engine, 2_000, 1_500).await;

    assert_eq!(peak(&engine), mib(Some(4_000), Some(1_500)));
}

#[tokio::test(start_paused = true)]
async fn an_unmeasured_reading_leaves_the_peak() {
    let mut engine = laya_loaded();
    survey_laya(&mut engine, 4_000, 1_500).await;

    sleep(SOON).await;
    let _ = engine.surveyed(Reading::empty(Instant::now()), |_| false);

    let snapshot = engine.snapshot();
    assert_eq!(laya_in(&snapshot).measured, Measured::default());
    assert_eq!(peak(&engine), mib(Some(4_000), Some(1_500)));
}

#[tokio::test(start_paused = true)]
async fn a_new_load_starts_the_peak_again() {
    let mut engine = laya_loaded();
    survey_laya(&mut engine, 4_000, 1_500).await;
    engine.feed(Event::RequestFinished {
        waiter: WaiterId(1),
        model: "laya".into(),
        client: MAC.into(),
    });
    sleep(Duration::from_secs(9 * 3600)).await;
    engine.feed(Event::Tick);
    assert!(matches!(engine.take_jobs().as_slice(), [Job::Unload(_)]));
    engine.finished(
        "laya".into(),
        Outcome::Unloaded {
            sheep: Some("laya".into()),
        },
    );

    ask_for_laya(&mut engine, 2);
    assert!(matches!(engine.take_jobs().as_slice(), [Job::Load(_)]));
    sleep(SOON).await;
    engine.finished("laya".into(), Outcome::Loaded);
    survey_laya(&mut engine, 1_000, 500).await;

    assert_eq!(peak(&engine), mib(Some(1_000), Some(500)));
}

#[tokio::test(start_paused = true)]
async fn a_model_not_holding_memory_reports_no_peak() {
    let mut engine = laya_loaded();
    survey_laya(&mut engine, 4_000, 1_500).await;
    engine.feed(Event::RequestFinished {
        waiter: WaiterId(1),
        model: "laya".into(),
        client: MAC.into(),
    });
    sleep(Duration::from_secs(9 * 3600)).await;
    engine.feed(Event::Tick);
    assert!(matches!(engine.take_jobs().as_slice(), [Job::Unload(_)]));
    engine.finished(
        "laya".into(),
        Outcome::Unloaded {
            sheep: Some("laya".into()),
        },
    );

    assert_eq!(engine.book.state(&"laya".into()), Some(State::Unloaded));
    assert_eq!(peak(&engine), Measured::default());
}

#[tokio::test(start_paused = true)]
async fn the_peak_is_not_counted_for_drift() {
    let mut engine = laya_loaded();
    // laya declares no VRAM, so 4000 MiB of it is drift.
    survey_laya(&mut engine, 4_000, 1_500).await;
    assert!(laya_in(&engine.snapshot()).drift);

    // The lamb leaves the GPU, and something else holds 2000 MiB.
    sleep(SOON).await;
    let gone = Reading {
        gpu: Some(
            gpu::reading(
                "2000 MiB, 24564 MiB\n",
                "4242, /usr/bin/python3, 2000 MiB\n",
            )
            .expect("readable"),
        ),
        ..laya_holding(Instant::now(), 0, 1_500)
    };
    let _ = engine.surveyed(gone, |_| false);

    let snapshot = engine.snapshot();
    assert!(!laya_in(&snapshot).drift);
    assert_eq!(peak(&engine).vram, Some(4_000 * MIB));
}
