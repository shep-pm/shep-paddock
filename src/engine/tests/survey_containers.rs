//! A model in a podman container measured by the survey, and podman failing, on a paused clock.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

use super::{survey::MIB, *};
pub(super) use crate::test_support::built::MAIN;
use crate::{
    engine::survey::{Blobs, Reading, read},
    survey::{ContainerRead, Measured, gpu, podman::Container},
    test_support::{
        FakeHost,
        built::{CLIENT, ENGINE, ENGINE_APP, contained},
    },
};

const GIB: u64 = 1 << 30;
pub(super) const CONTAINER: &str = "strata-qwen-iq3_s";

/// [`SHEEP_MODELS`] with iq3_s in a container.
pub(super) fn strata() -> Arc<Config> {
    config(&SHEEP_MODELS.replace(
        "backend = { sheep = \"iq3_s\" }",
        &format!("backend = {{ sheep = \"iq3_s\" }}\ncontainer = \"{CONTAINER}\""),
    ))
}

fn strata_engine() -> Engine {
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = Engine::new(strata(), Clock::new(), notify);
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "iq3_s".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    let _ = engine.take_jobs();
    engine.finished("iq3_s".into(), Outcome::Loaded);
    engine
}

fn strata_reading(asked: Instant, containers: Option<BTreeMap<String, ContainerRead>>) -> Reading {
    let sheep = ProcessInfo::builder(1, "iq3_s", ProcStatus::Online)
        .pid(Some(CLIENT))
        .lambs(Some(Vec::new()))
        .memory_bytes(Some(106 * MIB))
        .build();
    Reading {
        flock: Some(vec![sheep]),
        gpu: Some(gpu::reading("23900 MiB, 24564 MiB\n", ENGINE_APP).expect("readable")),
        containers,
        ..Reading::empty(asked)
    }
}

/// One survey of `host` with iq3_s in its container, and no bare lease.
async fn read_from(host: FakeHost) -> Reading {
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    timeout(
        BOUND,
        read(
            &backends,
            Rc::new(host),
            strata(),
            Blobs::new(),
            BTreeSet::new(),
        ),
    )
    .await
    .expect("a reading")
}

fn iq3_s(engine: &Engine) -> Measured {
    let snapshot = engine.snapshot();
    let view = snapshot
        .models
        .iter()
        .find(|view| view.name == ModelName::from("iq3_s"))
        .expect("iq3_s");
    view.measured
}

#[tokio::test(start_paused = true)]
async fn a_survey_reads_a_running_containers_pids_and_resident_memory() {
    let host = FakeHost::absent()
        .with_container(CONTAINER, Container::Running(MAIN))
        .with_cgroup(MAIN, &[MAIN, ENGINE])
        .with_rss(MAIN, 2 * MIB)
        .with_rss(ENGINE, 53 * GIB);
    let reading = read_from(host).await;
    assert_eq!(
        reading.containers,
        Some(BTreeMap::from([(CONTAINER.to_owned(), contained())]))
    );
    assert_eq!(reading.podman, None);
}

#[tokio::test(start_paused = true)]
async fn a_running_container_whose_cgroup_cannot_be_read_is_unread() {
    let host = FakeHost::absent().with_container(CONTAINER, Container::Running(MAIN));
    let reading = read_from(host).await;
    assert_eq!(reading.containers, None);
    assert_eq!(
        reading.podman.as_deref(),
        Some("the cgroup of \"strata-qwen-iq3_s\" could not be read")
    );
}

#[tokio::test(start_paused = true)]
async fn a_container_process_whose_memory_cannot_be_read_leaves_the_container_unread() {
    let host = FakeHost::absent()
        .with_container(CONTAINER, Container::Running(MAIN))
        .with_cgroup(MAIN, &[MAIN, ENGINE])
        .with_rss(MAIN, 2 * MIB);
    let reading = read_from(host).await;
    assert_eq!(reading.containers, None);
    assert_eq!(
        reading.podman.as_deref(),
        Some("the memory of \"strata-qwen-iq3_s\"'s process 1246137 could not be read")
    );
}

#[tokio::test(start_paused = true)]
async fn a_model_in_a_container_is_measured_with_what_the_container_holds() {
    let mut engine = strata_engine();
    sleep(SOON).await;
    let containers = BTreeMap::from([(CONTAINER.to_owned(), contained())]);
    let _ = engine.surveyed(strata_reading(Instant::now(), Some(containers)), |_| false);
    assert_eq!(
        iq3_s(&engine),
        Measured {
            vram: Some(23_800 * MIB),
            ram: Some(106 * MIB + 2 * MIB + 53 * GIB)
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_podman_that_cannot_be_asked_is_logged_once() {
    let mut engine = strata_engine();
    sleep(SOON).await;
    let failing = || Reading {
        podman: Some("podman could not be run, or did not answer".to_owned()),
        ..strata_reading(Instant::now(), None)
    };
    let said = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| line.contains("containers cannot be read"))
            .count()
    };

    assert_eq!(said(&engine.surveyed(failing(), |_| false)), 1);
    assert_eq!(
        iq3_s(&engine),
        Measured {
            vram: None,
            ram: Some(106 * MIB)
        },
        "measured by its sheep alone"
    );
    assert_eq!(said(&engine.surveyed(failing(), |_| false)), 0);
    let _ = engine.surveyed(
        strata_reading(Instant::now(), Some(BTreeMap::new())),
        |_| false,
    );
    assert_eq!(
        said(&engine.surveyed(failing(), |_| false)),
        1,
        "a fault that ends and comes back is told again"
    );
}
