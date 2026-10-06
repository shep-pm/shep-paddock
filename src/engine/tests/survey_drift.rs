//! What the engine logs from a survey: drift crossing both ways, and `nvidia-smi` it cannot read.

use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

use super::{
    survey::{MIB, ask_for_laya, laya_in, laya_reading},
    *,
};
use crate::{
    engine::survey::Reading,
    survey::gpu::{self, GpuParseError},
};

fn laya_loaded() -> Engine {
    let mut engine = engine();
    ask_for_laya(&mut engine, 1);
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    engine
}

/// Built, not captured: laya's lamb has left the GPU, and something else holds 2000 MiB.
fn laya_off_the_gpu(asked: Instant) -> Reading {
    Reading {
        gpu: Some(
            gpu::reading(
                "2000 MiB, 24564 MiB\n",
                "4242, /usr/bin/python3, 2000 MiB\n",
            )
            .expect("readable"),
        ),
        ..laya_reading(asked)
    }
}

#[tokio::test(start_paused = true)]
async fn drift_is_logged_and_shown_when_it_starts_and_when_it_stops() {
    let mut engine = laya_loaded();
    sleep(SOON).await;

    let started = engine.surveyed(laya_reading(Instant::now()));
    assert_eq!(
        started,
        [
            "paddock: laya is drifting: it measures 4000 MiB VRAM and 1504 MiB RAM \
          against no VRAM and 5120 MiB RAM declared"
        ]
    );
    assert!(laya_in(&engine.snapshot()).drift);

    sleep(SURVEY_EVERY).await;
    let stopped = engine.surveyed(laya_off_the_gpu(Instant::now()));
    assert_eq!(
        stopped,
        ["paddock: laya is back within its declared footprint"]
    );
    let snapshot = engine.snapshot();
    assert!(!laya_in(&snapshot).drift);
    assert_eq!(snapshot.unaccounted_vram, Some(2_000 * MIB));
}

#[tokio::test(start_paused = true)]
async fn nvidia_smi_that_cannot_be_read_is_logged_once_until_it_changes() {
    let mut engine = engine();
    let unreadable = |error: GpuParseError| Reading {
        unreadable: Some(error),
        ..Reading::empty(Instant::now())
    };
    let line = GpuParseError::Line {
        line: "[N/A], [N/A]".to_owned(),
    };

    assert_eq!(
        engine.surveyed(unreadable(GpuParseError::NoGpu)),
        ["paddock: nvidia-smi listed no GPU"]
    );
    assert_eq!(
        engine.surveyed(unreadable(GpuParseError::NoGpu)),
        Vec::<String>::new()
    );
    assert_eq!(
        engine.surveyed(unreadable(line.clone())),
        ["paddock: nvidia-smi printed a line the dog cannot read: [N/A], [N/A]"]
    );
    assert_eq!(
        engine.surveyed(Reading::empty(Instant::now())),
        Vec::<String>::new()
    );
    assert_eq!(
        engine.surveyed(unreadable(line)).len(),
        1,
        "logged again once it came back"
    );
}

#[tokio::test(start_paused = true)]
async fn a_figure_the_survey_could_not_read_keeps_its_drift_without_a_line() {
    let mut engine = laya_loaded();
    sleep(SOON).await;
    assert_eq!(engine.surveyed(laya_reading(Instant::now())).len(), 1);

    let without_gpu = |asked| Reading {
        gpu: None,
        ..laya_reading(asked)
    };
    let without_flock = |asked| Reading {
        flock: None,
        ..laya_reading(asked)
    };
    for unread in [without_gpu, without_flock] {
        sleep(SURVEY_EVERY).await;
        assert_eq!(
            engine.surveyed(unread(Instant::now())),
            Vec::<String>::new()
        );
        assert!(
            laya_in(&engine.snapshot()).drift,
            "still drifting as far as is known"
        );
    }

    sleep(SURVEY_EVERY).await;
    assert_eq!(
        engine.surveyed(laya_off_the_gpu(Instant::now())),
        ["paddock: laya is back within its declared footprint"]
    );
}

#[tokio::test(start_paused = true)]
async fn ram_drift_comes_and_goes_without_nvidia_smi() {
    let mut engine = laya_loaded();
    let in_ram = |ram_mib: u64| {
        let laya = ProcessInfo::builder(1, "laya", ProcStatus::Online)
            .pid(Some(7))
            .memory_bytes(Some(ram_mib * MIB))
            .build();
        Reading {
            flock: Some(vec![laya]),
            ..Reading::empty(Instant::now())
        }
    };
    sleep(SOON).await;

    assert_eq!(
        engine.surveyed(in_ram(6_000)).len(),
        1,
        "over 5120 MiB by more than 10%"
    );
    sleep(SURVEY_EVERY).await;
    assert_eq!(
        engine.surveyed(in_ram(1_504)),
        ["paddock: laya is back within its declared footprint"]
    );
}
