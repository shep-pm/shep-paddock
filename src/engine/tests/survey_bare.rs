//! A bare job measured by the survey, on a paused clock.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use super::{survey::MIB, *};
use crate::{
    engine::survey::{Blobs, Reading, read},
    footprint::{Footprint, Vram},
    survey::{Measured, gpu},
    test_support::{
        FakeHost,
        built::{BARE_APPS, bare_parents},
    },
};

fn bare_engine(declared_gib: u64, pid: Option<u32>) -> Engine {
    let mut engine = engine();
    let ask = LeaseAsk {
        lease: LeaseId(1),
        client: BENCH.into(),
        leased: Leased::Bare {
            footprint: Footprint {
                vram: Vram::Bytes(declared_gib << 30),
                ram: 1 << 30,
            },
            pid,
        },
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: None,
        reclaimable: false,
        release_if_idle: None,
    };
    engine.feed(Event::LeaseAsked {
        waiter: WaiterId(1),
        ask,
    });
    engine
}

fn bare_reading(asked: Instant) -> Reading {
    Reading {
        gpu: Some(gpu::reading("9000 MiB, 24564 MiB\n", BARE_APPS).expect("readable")),
        parents: bare_parents(),
        ..Reading::empty(asked)
    }
}

#[tokio::test(start_paused = true)]
async fn a_bare_jobs_gpu_memory_is_its_pid_and_every_process_below_it() {
    let mut engine = bare_engine(8, Some(4321));
    let _ = engine.surveyed(bare_reading(Instant::now()), |_| false);
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.leases[0].measured,
        Measured {
            vram: Some(7_000 * MIB),
            ram: None
        }
    );
    assert!(!snapshot.leases[0].drift);
    assert_eq!(snapshot.unaccounted_vram, Some(2_000 * MIB));
}

#[tokio::test(start_paused = true)]
async fn a_bare_job_over_its_declared_vram_drifts_and_says_so_once() {
    let mut engine = bare_engine(4, Some(4321));
    let lines = engine.surveyed(bare_reading(Instant::now()), |_| false);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("paddock: L1 is drifting")),
        "{lines:?}"
    );
    assert!(engine.snapshot().leases[0].drift);
    assert!(
        engine
            .surveyed(bare_reading(Instant::now()), |_| false)
            .is_empty()
    );
}

#[tokio::test(start_paused = true)]
async fn a_bare_lease_without_a_pid_has_its_declared_vram_taken_off_unaccounted() {
    let mut engine = bare_engine(4, None);
    let _ = engine.surveyed(bare_reading(Instant::now()), |_| false);
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.leases[0].measured, Measured::default());
    assert_eq!(snapshot.unaccounted_vram, Some(9_000 * MIB - 4_096 * MIB));
}

#[tokio::test(start_paused = true)]
async fn a_survey_walks_each_gpu_process_up_to_a_bare_leases_pid() {
    let host = FakeHost::printing(
        "9000 MiB, 24564 MiB\n",
        "5002, /usr/bin/python3, 1000 MiB\n7000, /usr/bin/other, 500 MiB\n",
    )
    .with_parent(5002, 5001)
    .with_parent(5001, 4321)
    .with_parent(7000, 6000)
    .with_parent(6000, 1);
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let reading = timeout(
        BOUND,
        read(
            &backends,
            Rc::new(host),
            config(SHEEP_MODELS),
            Blobs::new(),
            BTreeSet::from([4321]),
        ),
    )
    .await
    .expect("a reading");
    assert_eq!(
        reading.parents,
        BTreeMap::from([(5002, 5001), (5001, 4321), (7000, 6000), (6000, 1)])
    );
}

#[tokio::test(start_paused = true)]
async fn a_survey_walking_a_looping_parent_chain_finishes() {
    let host = FakeHost::printing(
        "1000 MiB, 24564 MiB\n",
        "5001, /usr/bin/python3, 1000 MiB\n",
    )
    .with_parent(5001, 5002)
    .with_parent(5002, 5001);
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let reading = timeout(
        BOUND,
        read(
            &backends,
            Rc::new(host),
            config(SHEEP_MODELS),
            Blobs::new(),
            BTreeSet::from([4321]),
        ),
    )
    .await
    .expect("the walk ends");
    assert_eq!(
        reading.parents,
        BTreeMap::from([(5001, 5002), (5002, 5001)])
    );
}

#[tokio::test(start_paused = true)]
async fn a_survey_begun_with_no_bare_lease_still_walks_up_for_one_granted_meanwhile() {
    let host = FakeHost::printing(
        "9000 MiB, 24564 MiB\n",
        "5002, /usr/bin/python3, 1000 MiB\n",
    )
    .with_parent(5002, 5001)
    .with_parent(5001, 4321);
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let reading = timeout(
        BOUND,
        read(
            &backends,
            Rc::new(host),
            config(SHEEP_MODELS),
            Blobs::new(),
            BTreeSet::new(),
        ),
    )
    .await
    .expect("a reading");
    assert_eq!(
        reading.parents,
        BTreeMap::from([(5002, 5001), (5001, 4321)])
    );
}
