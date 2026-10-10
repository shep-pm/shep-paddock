//! The engine against a fake shepherd, on a paused clock unless a test says otherwise.

use std::{collections::VecDeque, future::Future, sync::Arc, time::Duration};

use shep_client::dogs::Stop;
use tokio::{
    sync::mpsc,
    task::{LocalSet, spawn_local},
    time::{Instant, sleep, timeout},
};

use super::{
    Admission, Clock, EngineHandle, InFlight, LeaseEvent, LeaseEvents, LeaseRefused, LeaseRequest,
    SURVEY_EVERY, Start, channel, channel_on, lease_channel, run,
    state::{Engine, Job, Outcome},
};
use crate::{
    backend::Backends,
    book::{
        Action, Event, Hold, LeaseAsk, LeaseId, Leased, Priority, Reason, RestoredLease, State,
        WaiterId,
    },
    config::{Config, ModelName},
    shepherd::{ProcessEvent, ProcessKind},
    test_support::{Call, FakeShepherd, config, fake_http},
};

mod discovered;
mod idle;
mod leases;
mod placements;
mod process;
mod records;
mod removed;
mod requests;
mod restart;
mod saved_models;
mod saving;
mod strays;
mod survey;
mod survey_drift;
mod survey_failures;
mod survey_pace;
mod survey_races;
mod survey_strays;

/// The spec's sheep models without ready checks, so a load is done once its
/// sheep comes online, which the fake says as its restart answers. No test
/// needs an HTTP server or real time.
const SHEEP_MODELS: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"

[[clients]]
name = "bench-01"
key = "k-bench"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
apis = ["openai", "anthropic"]
vram = "all"
ram = "37G"
idle = "2h"

[models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144" } }
url = "http://127.0.0.1:8080"
apis = ["openai", "anthropic"]
vram = "all"
ram = "44G"
idle = "2h"

[models.iq3_s]
backend = { sheep = "iq3_s" }
url = "http://127.0.0.1:8081"
apis = ["openai", "anthropic"]
vram = "all"
ram = "55G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;

// Longer than any wait a test makes the engine do, two load timeouts included.
const BOUND: Duration = Duration::from_secs(3600);
// Far shorter than any timer the engine runs, so what happens within it was not a timer.
const SOON: Duration = Duration::from_secs(1);
// Longer than any test waits, so no request is refused for waiting too long.
const MAX_WAIT: Duration = Duration::from_secs(1800);
const MAC: &str = "mac-sessions";
const QWEN: &str = "qwen3.8:27b";
const BENCH: &str = "bench-01";

/// Runs `body` beside an engine on `shepherd`, both on this test's thread.
async fn with_engine<F, Fut>(config: Arc<Config>, shepherd: FakeShepherd, body: F)
where
    F: FnOnce(EngineHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    with_engine_from(config, shepherd, Start::default(), body).await;
}

/// As [`with_engine`], with the engine starting from `start`. The engine stops when `body` ends.
async fn with_engine_from<F, Fut>(
    config: Arc<Config>,
    shepherd: FakeShepherd,
    start: Start,
    body: F,
) where
    F: FnOnce(EngineHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    with_engine_on(Clock::new(), config, shepherd, start, body).await;
}

/// As [`with_engine_from`], with the engine's time read from `clock`.
async fn with_engine_on<F, Fut>(
    clock: Clock,
    config: Arc<Config>,
    shepherd: FakeShepherd,
    start: Start,
    body: F,
) where
    F: FnOnce(EngineHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    let (handle, inbox) = channel_on(clock);
    let backends = Backends::new(shepherd, crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(run(config, backends, start, inbox, Stop::never()));
    local.run_until(body(handle)).await;
}

async fn admit(engine: EngineHandle, model: &'static str) -> Admission {
    engine
        .admit(MAC.into(), model.into(), Priority::Interactive, MAX_WAIT)
        .await
}

async fn forwarded(engine: &EngineHandle, model: &'static str) -> InFlight {
    match timeout(BOUND, admit(engine.clone(), model)).await {
        Ok(Admission::Forward(in_flight)) => in_flight,
        other => panic!("{model} was not forwarded: {other:?}"),
    }
}

/// Waits for `done` to hold, failing the test if it never does.
async fn until<F, Fut>(what: &str, done: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    until_within(BOUND, what, done).await;
}

/// Waits up to `bound` for `done` to hold, failing the test if it does not.
async fn until_within<F, Fut>(bound: Duration, what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let waited = timeout(bound, async {
        while !done().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what} did not happen within {bound:?}");
}

async fn state_of(engine: &EngineHandle, model: &str) -> Option<State> {
    let snapshot = engine.snapshot().await;
    let model = ModelName::from(model);
    snapshot
        .models
        .iter()
        .find(|view| view.name == model)
        .map(|view| view.state)
}

async fn until_state(engine: &EngineHandle, model: &str, state: State) {
    until(&format!("{model} turning {state:?}"), || async {
        state_of(engine, model).await == Some(state)
    })
    .await;
}

async fn until_called(shepherd: &FakeShepherd, call: Call) {
    until(&format!("{call:?}"), || async {
        shepherd.calls().contains(&call)
    })
    .await;
}

fn crash(sheep: &str, kind: ProcessKind, manually: bool) -> ProcessEvent {
    ProcessEvent {
        sheep: sheep.to_owned(),
        kind,
        manually,
        pid: None,
    }
}

fn online(sheep: &str) -> ProcessEvent {
    ProcessEvent {
        sheep: sheep.to_owned(),
        kind: ProcessKind::Online,
        manually: true,
        pid: None,
    }
}

fn calls_of(shepherd: &FakeShepherd, call: &Call) -> usize {
    shepherd.calls().iter().filter(|made| *made == call).count()
}

fn stops(shepherd: &FakeShepherd) -> usize {
    shepherd
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Stop(_)))
        .count()
}

fn lease_on(model: &str, hold: Hold) -> LeaseRequest {
    LeaseRequest {
        model: model.into(),
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold,
        note: None,
        reclaimable: false,
        release_if_idle: None,
    }
}

/// Reads the stream up to its grant, failing on anything that ends the wait otherwise.
async fn granted(events: &mut LeaseEvents) -> LeaseId {
    loop {
        match timeout(BOUND, events.recv()).await {
            Ok(Some(LeaseEvent::Granted { lease })) => return lease,
            Ok(Some(LeaseEvent::Waiting { .. })) => {}
            other => panic!("the lease was not granted: {other:?}"),
        }
    }
}

fn engine() -> Engine {
    let (notify, _) = mpsc::unbounded_channel();
    Engine::new(config(SHEEP_MODELS), Clock::new(), notify)
}

#[tokio::test(start_paused = true)]
async fn the_engine_returns_once_stopped() {
    let (_handle, inbox) = channel();
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let (stop, request) = Stop::new();
    request.request();
    let local = LocalSet::new();
    let ran = local
        .run_until(timeout(
            BOUND,
            run(
                config(SHEEP_MODELS),
                backends,
                Start::default(),
                inbox,
                stop,
            ),
        ))
        .await;
    assert!(ran.is_ok(), "the engine ran on after a stop");
}
