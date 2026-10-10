//! Container strays and the unload that stops them, on a paused clock.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use super::{
    survey::{surveyed_every, view_of},
    survey_containers::{CONTAINER, MAIN, strata},
    *,
};
use crate::{
    engine::survey::Reading,
    survey::{ContainerRead, podman::Container},
    test_support::{FakeContainers, FakeHost},
};

/// Runs `body` beside an engine whose containers are `containers`.
async fn with_podman<F, Fut>(
    shepherd: FakeShepherd,
    containers: FakeContainers,
    start: Start,
    body: F,
) where
    F: FnOnce(EngineHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    let (handle, inbox) = channel_on(Clock::new());
    let backends = Backends::new(shepherd, crate::outbound::http_client())
        .with_containers(Rc::new(containers));
    let local = LocalSet::new();
    local.spawn_local(run(strata(), backends, start, inbox, Stop::never()));
    local.run_until(body(handle)).await;
}

/// A host whose container runs, with its main process alone in its cgroup.
fn running_host() -> FakeHost {
    FakeHost::absent()
        .with_container(CONTAINER, Container::Running(MAIN))
        .with_cgroup(MAIN, &[MAIN])
        .with_rss(MAIN, 0)
}

fn running() -> BTreeMap<String, ContainerRead> {
    BTreeMap::from([(
        CONTAINER.to_owned(),
        ContainerRead {
            pids: BTreeSet::from([MAIN]),
            ram: 0,
        },
    )])
}

#[tokio::test(start_paused = true)]
async fn a_container_running_while_its_model_is_unloaded_is_a_stray() {
    let host = running_host();
    let shepherd = FakeShepherd::new();
    let containers = FakeContainers::after(&shepherd);
    with_podman(
        shepherd,
        containers,
        surveyed_every(host, SURVEY_EVERY),
        |engine| async move {
            until("the stray counted", || async {
                state_of(&engine, "iq3_s").await == Some(State::Loaded)
            })
            .await;
            assert!(view_of(&engine, "iq3_s").await.expect("iq3_s").stray);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn unloading_a_container_stray_stops_the_sheep_then_the_container() {
    let host = running_host();
    let shepherd = FakeShepherd::new();
    let containers = FakeContainers::after(&shepherd);
    let seen = containers.clone();
    with_podman(
        shepherd,
        containers,
        surveyed_every(host, SURVEY_EVERY),
        |engine| async move {
            until("the stray counted", || async {
                state_of(&engine, "iq3_s").await == Some(State::Loaded)
            })
            .await;
            drop(forwarded(&engine, "iq2_xs").await);
            assert_eq!(
                seen.stopped(),
                vec![(CONTAINER.to_owned(), 1)],
                "the container stopped once, after the sheep"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_container_still_running_after_its_stray_is_unloaded_is_counted_again() {
    let host = running_host();
    let shepherd = FakeShepherd::new();
    let containers = FakeContainers::after(&shepherd);
    with_podman(
        shepherd,
        containers,
        surveyed_every(host, SURVEY_EVERY),
        |engine| async move {
            until("the stray counted", || async {
                state_of(&engine, "iq3_s").await == Some(State::Loaded)
            })
            .await;
            drop(forwarded(&engine, "iq2_xs").await);
            until_state(&engine, "iq3_s", State::Unloaded).await;
            until("the stray counted again", || async {
                state_of(&engine, "iq3_s").await == Some(State::Loaded)
            })
            .await;
            assert!(view_of(&engine, "iq3_s").await.expect("iq3_s").stray);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stray_whose_container_runs_on_is_not_forgotten_when_its_sheep_is_gone() {
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(strata(), Clock::new(), notify);
    let reading = |containers: Option<BTreeMap<String, ContainerRead>>| Reading {
        flock: Some(Vec::new()),
        containers,
        ..Reading::empty(Instant::now())
    };

    assert_eq!(
        engine.surveyed(reading(Some(running())), |_| false),
        vec![format!(
            "paddock: container {CONTAINER} is running without the dog; counting it as iq3_s"
        )]
    );
    sleep(SOON).await;
    assert!(
        engine
            .surveyed(reading(Some(running())), |_| false)
            .is_empty(),
        "its sheep is gone, but its container runs"
    );
    assert!(
        engine.surveyed(reading(None), |_| false).is_empty(),
        "nothing is forgotten while podman cannot be asked"
    );
    assert_eq!(
        engine.surveyed(reading(Some(BTreeMap::new())), |_| false),
        vec!["paddock: iq3_s, a stray, no longer runs; forgetting it".to_owned()]
    );
}
