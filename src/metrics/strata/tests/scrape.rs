//! Which models are scraped and how, against the fake HTTP server, and the reporter beside it.

use std::{rc::Rc, sync::Arc};

use shep_client::dogs::Stop;
use tokio::{
    net::TcpListener,
    sync::watch,
    task::{LocalSet, spawn_local},
    time::sleep,
};

use super::*;
use crate::{
    backend::Backends,
    book::Found,
    discover::Discovered,
    engine::{self, Start},
    metrics::{
        REPORT_EVERY, report,
        tests::{Recorder, model as view, snapshot},
    },
    outbound::http_client,
    test_support::{FakeShepherd, Route, fake_http},
};

/// An ollama model, a plain sheep model and two sheep models marked for Strata, all at `base`.
/// `extra` goes on `marked`.
fn flock(base: &str, extra: &str) -> Config {
    let text = format!(
        r#"
[host]
vram = "24G"
ram = "62G"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models.ollama-model]
backend = "ollama"
name = "qwen3"
vram = "1G"
idle = "2h"

[models.plain]
backend = {{ sheep = "plain" }}
url = "{base}"
ram = "1G"
idle = "2h"

[models.marked]
backend = {{ sheep = "marked" }}
url = "{base}"
ram = "1G"
idle = "2h"
metrics = "strata"
{extra}

[models.asleep]
backend = {{ sheep = "asleep" }}
url = "{base}"
ram = "1G"
idle = "2h"
metrics = "strata"
"#
    );
    Config::from_toml(&text).expect("a flock that parses")
}

/// A loopback address nothing listens on
const NOBODY: &str = "http://127.0.0.1:1";

fn snapshot_of(states: &[(&str, State)]) -> Snapshot {
    let models = states.iter().map(|(name, state)| view(name, *state));
    snapshot(models.collect(), Vec::new(), Vec::new())
}

fn marked_is(state: State) -> Snapshot {
    snapshot_of(&[("marked", state)])
}

async fn read(scraper: &mut Scraper, snapshot: &Snapshot, config: &Config) -> Vec<Sample> {
    scraper.samples(snapshot, config, &mut Vec::new()).await
}

fn ok() -> Vec<Route> {
    vec![("GET", "/metrics", vec![(200, CAPTURED)])]
}

#[tokio::test]
async fn only_a_loaded_model_marked_strata_is_scraped() {
    let (base, http) = fake_http(ok());
    let config = flock(&base, "");
    let snapshot = snapshot_of(&[
        ("ollama-model", State::Loaded),
        ("plain", State::Loaded),
        ("marked", State::Loaded),
        ("asleep", State::Unloaded),
    ]);

    let samples = read(&mut Scraper::new(http_client()), &snapshot, &config).await;

    let seen = http.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/metrics")
    );
    assert!(!samples.is_empty());
    assert!(
        samples
            .iter()
            .all(|sample| sample.labels["model"] == "marked")
    );
}

#[tokio::test]
async fn a_stray_the_config_does_not_name_is_not_scraped() {
    let (base, http) = fake_http(ok());
    let config = flock(&base, "");

    let samples = read(
        &mut Scraper::new(http_client()),
        &snapshot_of(&[("sheep:iq3_s", State::Loaded)]),
        &config,
    )
    .await;

    assert!(samples.is_empty());
    assert!(http.seen().is_empty());
}

#[tokio::test]
async fn the_models_key_is_sent_as_the_bearer() {
    let (base, http) = fake_http(ok());
    let config = flock(&base, "key = \"k-strata\"");

    read(
        &mut Scraper::new(http_client()),
        &marked_is(State::Loaded),
        &config,
    )
    .await;

    assert_eq!(
        http.seen()[0].authorization.as_deref(),
        Some("Bearer k-strata")
    );
}

#[tokio::test]
async fn a_model_with_no_key_sends_no_authorization() {
    let (base, http) = fake_http(ok());
    let config = flock(&base, "");

    read(
        &mut Scraper::new(http_client()),
        &marked_is(State::Loaded),
        &config,
    )
    .await;

    assert_eq!(http.seen()[0].authorization, None);
}

#[tokio::test]
async fn a_failed_scrape_sends_nothing_and_logs_once_until_one_succeeds() {
    let answers = vec![
        (503, "busy"),
        (200, "<html>not metrics</html>"),
        (200, CAPTURED),
        (404, ""),
    ];
    let (base, _http) = fake_http(vec![("GET", "/metrics", answers)]);
    let config = flock(&base, "key = \"k-strata\"");
    let snapshot = marked_is(State::Loaded);
    let mut scraper = Scraper::new(http_client());
    let mut log = Vec::new();

    let first = scraper.samples(&snapshot, &config, &mut log).await;
    let second = scraper.samples(&snapshot, &config, &mut log).await;

    assert!(first.is_empty() && second.is_empty());
    let logged = String::from_utf8(log.clone()).unwrap();
    assert_eq!(logged.lines().count(), 1, "{logged}");
    assert!(
        logged.contains("marked") && logged.contains("503"),
        "{logged}"
    );
    for secret in ["k-strata", &base] {
        assert!(!logged.contains(secret), "{logged}");
    }

    let healed = scraper.samples(&snapshot, &config, &mut log).await;
    assert!(!healed.is_empty());
    assert_eq!(String::from_utf8(log.clone()).unwrap(), logged);

    let broken = scraper.samples(&snapshot, &config, &mut log).await;
    assert!(broken.is_empty());
    assert_eq!(String::from_utf8(log).unwrap().lines().count(), 2);
}

#[tokio::test]
async fn a_refused_connection_is_a_failed_scrape() {
    let mut log = Vec::new();

    let samples = Scraper::new(http_client())
        .samples(&marked_is(State::Loaded), &flock(NOBODY, ""), &mut log)
        .await;

    assert!(samples.is_empty());
    assert_eq!(String::from_utf8(log).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn a_failed_round_leaves_the_gauges_alone() {
    let answers = vec![(200, CAPTURED), (500, "")];
    let (base, _http) = fake_http(vec![("GET", "/metrics", answers)]);
    let config = flock(&base, "");
    let snapshot = marked_is(State::Loaded);
    let mut scraper = Scraper::new(http_client());
    read(&mut scraper, &snapshot, &config).await;

    let failed = read(&mut scraper, &snapshot, &config).await;

    assert!(failed.is_empty(), "{failed:?}");
}

#[tokio::test]
async fn an_unloaded_model_sends_its_strata_gauges_once_at_zero_and_not_its_counters() {
    let (base, _http) = fake_http(ok());
    let config = flock(&base, "");
    let mut scraper = Scraper::new(http_client());
    let loaded = read(&mut scraper, &marked_is(State::Loaded), &config).await;
    let gauges = loaded
        .iter()
        .filter(|s| s.series.kind == Kind::Gauge)
        .count();

    let unloaded = read(&mut scraper, &marked_is(State::Unloaded), &config).await;
    let after = read(&mut scraper, &marked_is(State::Unloaded), &config).await;

    assert_eq!(unloaded.len(), gauges, "{unloaded:?}");
    assert_eq!(gauges, 10);
    for sample in &unloaded {
        assert_eq!(sample.series.kind, Kind::Gauge, "{sample:?}");
        assert_eq!(sample.value, 0.0, "{sample:?}");
        assert_eq!(sample.labels["model"], "marked");
    }
    let states: Vec<_> = unloaded.iter().filter(|s| s.series == STATE).collect();
    assert_eq!(states.len(), 4);
    assert!(after.is_empty(), "{after:?}");
}

#[tokio::test]
async fn a_model_no_longer_marked_gets_the_same_one_zero() {
    let (base, _http) = fake_http(ok());
    let mut scraper = Scraper::new(http_client());
    let snapshot = marked_is(State::Loaded);
    read(&mut scraper, &snapshot, &flock(&base, "")).await;
    let mut config = flock(&base, "");
    let marked = config.models.get_mut(&ModelName::from("marked")).unwrap();
    marked.metrics = None;

    let zeroed = read(&mut scraper, &snapshot, &config).await;
    let after = read(&mut scraper, &snapshot, &config).await;

    assert_eq!(zeroed.len(), 10, "{zeroed:?}");
    assert!(zeroed.iter().all(|sample| sample.value == 0.0));
    assert!(after.is_empty());
}

/// A server that accepts connections and never answers
async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    base
}

/// Runs `body` beside a real engine that holds `marked` loaded, with the reporter's config
async fn with_marked_loaded<F, Fut>(config: Config, body: F)
where
    F: FnOnce(engine::EngineHandle, watch::Receiver<Arc<Config>>) -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    let config = Arc::new(config);
    let marked = ModelName::from("marked");
    let start = Start {
        discovered: Discovered {
            loaded: vec![Found {
                model: marked.clone(),
                footprint: config.models[&marked].footprint,
                placement: None,
                stray: false,
            }],
            ..Discovered::default()
        },
        ..Start::default()
    };
    let (handle, inbox) = engine::channel();
    let shepherd = FakeShepherd::new();
    shepherd.running("marked");
    let backends = Backends::new(shepherd, http_client());
    let (_sender, receiver) = watch::channel(Arc::clone(&config));
    let local = LocalSet::new();
    local.spawn_local(engine::run(config, backends, start, inbox, Stop::never()));
    local.run_until(body(handle, receiver)).await;
}

fn own_series(recorder: &Recorder) -> usize {
    let sent = recorder.sent.borrow();
    sent.iter()
        .filter(|sample| sample.series.name == "paddock_model_loaded")
        .count()
}

fn strata_series(recorder: &Recorder) -> usize {
    let sent = recorder.sent.borrow();
    sent.iter()
        .filter(|sample| sample.series.name.starts_with("paddock_strata_"))
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_scrape_past_its_timeout_does_not_hold_up_the_report() {
    let config = flock(&silent().await, "");
    with_marked_loaded(config, |handle, receiver| async move {
        let recorder = Rc::new(Recorder::new(true));
        let (stop, _request) = Stop::new();
        let sink = Rc::clone(&recorder);
        let reporting = spawn_local(async move { report(&*sink, &handle, receiver, stop).await });

        sleep(Duration::from_secs(1)).await;
        let first = own_series(&recorder);
        assert!(first > 0, "the dog's own series waited on the backend");

        sleep(REPORT_EVERY + SCRAPE_TIMEOUT).await;
        assert_eq!(
            own_series(&recorder),
            2 * first,
            "the second round was late"
        );
        assert_eq!(strata_series(&recorder), 0);
        reporting.abort();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_scrape_still_sends_the_dogs_own_series() {
    with_marked_loaded(flock(NOBODY, ""), |handle, receiver| async move {
        let recorder = Rc::new(Recorder::new(true));
        let (stop, _request) = Stop::new();
        let sink = Rc::clone(&recorder);
        let reporting = spawn_local(async move { report(&*sink, &handle, receiver, stop).await });

        sleep(REPORT_EVERY + Duration::from_secs(5)).await;

        assert!(own_series(&recorder) > 0);
        let loaded = recorder.sent.borrow().iter().any(|s| {
            s.series.name == "paddock_model_loaded"
                && s.labels["model"] == "marked"
                && s.value == 1.0
        });
        assert!(loaded, "the loaded model was not reported");
        assert_eq!(strata_series(&recorder), 0);
        reporting.abort();
    })
    .await;
}

#[tokio::test]
async fn the_reporter_sends_a_loaded_models_strata_series_beside_its_own() {
    let (base, _http) = fake_http(ok());
    with_marked_loaded(flock(&base, ""), |handle, receiver| async move {
        let recorder = Rc::new(Recorder::new(true));
        let (stop, request) = Stop::new();
        let sink = Rc::clone(&recorder);
        let reporting = spawn_local(async move { report(&*sink, &handle, receiver, stop).await });

        for _ in 0..200 {
            if strata_series(&recorder) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(strata_series(&recorder), 18);
        assert!(own_series(&recorder) > 0);
        request.request();
        let _ = reporting.await;
    })
    .await;
}
