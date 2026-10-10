//! The status and model list as JSON, and both routes over a real loopback socket with the
//! engine behind them, so the route tests run on real time. Every await is bounded by `LIMIT`.

use std::{future::Future, sync::Arc, time::Duration};

use serde_json::json;
use shep_client::dogs::Stop;
use tokio::{
    net::TcpListener,
    sync::watch,
    task::{LocalSet, spawn_local},
    time::{sleep, timeout},
};

use super::*;
use crate::{
    backend::Backends,
    book::{
        Hold, LeaseId, LeaseView, LoadError, ModelView, Moment, Priority, Reason, State,
        WaiterKind, WaiterView,
    },
    config::{ClientName, ModelName},
    engine::{EngineHandle, Start, channel, run},
    footprint::{Footprint, Vram},
    http::{Shared, Timeouts, serve},
    survey::Measured,
    test_support::{FakeShepherd, config},
};

mod placements;

// Past any one step a test waits on: a load on the fake shepherd, or one request.
const LIMIT: Duration = Duration::from_secs(10);

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    match timeout(LIMIT, future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out: {what}"),
    }
}

fn clock() -> Clock {
    Clock::started_at("2026-10-04T10:00:00Z".parse().expect("a timestamp"))
}

fn view(name: &str, state: State, last_used: Moment) -> ModelView {
    ModelView {
        name: ModelName::from(name),
        state,
        in_flight: 0,
        last_used,
        held_by: Vec::new(),
        unknown: false,
        stray: false,
        placement: None,
        footprint: Footprint {
            vram: Vram::None,
            ram: 0,
        },
        measured: Measured::default(),
        drift: false,
    }
}

fn snapshot(clock: &Clock) -> Snapshot {
    let at = |text: &str| clock.moment_of(text.parse().expect("a timestamp"));
    let bench = ClientName::from("bench-01");
    let mac = ClientName::from("mac-sessions");
    Snapshot {
        models: vec![
            ModelView {
                in_flight: 1,
                held_by: vec![bench.clone()],
                ..view("iq2_xs", State::Loaded, at("2026-10-04T09:00:00Z"))
            },
            view("laya", State::Unloaded, Moment(0)),
            ModelView {
                unknown: true,
                ..view("sheep:iq3_s", State::Loaded, at("2026-10-04T08:30:00.5Z"))
            },
        ],
        leases: vec![
            LeaseView {
                id: LeaseId(1),
                client: bench.clone(),
                model: ModelName::from("iq2_xs"),
                priority: Priority::Batch,
                since: at("2026-10-04T08:00:00Z"),
                expected_until: Some(at("2026-10-04T16:00:00Z")),
                note: Some("strata h2h run 3".to_owned()),
                hold: Hold::Connection,
                attached: false,
                reclaimable: false,
                last_activity: at("2026-10-04T08:00:00Z"),
                in_use: false,
                release_if_idle: None,
            },
            LeaseView {
                id: LeaseId(2),
                client: mac.clone(),
                model: ModelName::from("iq2_xs"),
                priority: Priority::Interactive,
                since: at("2026-10-04T09:45:00Z"),
                expected_until: None,
                note: None,
                hold: Hold::Heartbeat {
                    ttl: Duration::from_secs(60),
                },
                attached: true,
                reclaimable: false,
                last_activity: at("2026-10-04T09:45:00Z"),
                in_use: false,
                release_if_idle: None,
            },
        ],
        waiters: vec![
            WaiterView {
                client: mac,
                model: ModelName::from("qwen3.8:27b"),
                kind: WaiterKind::Request,
                priority: Priority::Interactive,
                since: at("2026-10-04T09:59:00Z"),
                reason: Some(Reason::Held {
                    model: ModelName::from("iq2_xs"),
                    client: bench.clone(),
                    lease: LeaseId(1),
                    since: at("2026-10-04T08:00:00Z"),
                    until: Some(at("2026-10-04T16:00:00Z")),
                    idle_since: Some(at("2026-10-04T08:00:00Z")),
                }),
                estimate: Some(at("2026-10-04T16:00:00Z")),
            },
            WaiterView {
                client: bench,
                model: ModelName::from("iq3_s"),
                kind: WaiterKind::Lease,
                priority: Priority::Batch,
                since: at("2026-10-04T09:59:30Z"),
                reason: None,
                estimate: None,
            },
        ],
        errors: vec![LoadError {
            model: ModelName::from("iq3_s"),
            at: at("2026-10-04T07:00:00Z"),
            error: "out of memory".to_owned(),
        }],
        declared: Footprint {
            vram: Vram::Bytes(25_757_220_864),
            ram: 39_728_447_488,
        },
        unaccounted_vram: None,
    }
}

#[tokio::test(start_paused = true)]
async fn status_reports_bytes_and_rfc3339() {
    let clock = clock();
    let host = Host {
        vram: 25_757_220_864,
        ram: 66_520_760_320,
    };

    let body = status_body(&snapshot(&clock), &host, &clock);

    assert_eq!(
        body,
        json!({
            "host": {
                "vram_bytes": 25_757_220_864_u64,
                "ram_bytes": 66_520_760_320_u64,
                "vram_declared_bytes": 25_757_220_864_u64,
                "ram_declared_bytes": 39_728_447_488_u64,
            },
            "models": [
                { "model": "iq2_xs", "state": "loaded", "in_flight": 1,
                  "last_used": "2026-10-04T09:00:00Z", "held_by": ["bench-01"], "unknown": false,
                  "placement": null, "stray": false,
                  "measured": { "vram_bytes": null, "ram_bytes": null }, "drift": false },
                { "model": "laya", "state": "unloaded", "in_flight": 0,
                  "last_used": null, "held_by": [], "unknown": false,
                  "placement": null, "stray": false,
                  "measured": { "vram_bytes": null, "ram_bytes": null }, "drift": false },
                { "model": "sheep:iq3_s", "state": "loaded", "in_flight": 0,
                  "last_used": "2026-10-04T08:30:00.5Z", "held_by": [], "unknown": true,
                  "placement": null, "stray": false,
                  "measured": { "vram_bytes": null, "ram_bytes": null }, "drift": false },
            ],
            "leases": [
                { "id": "L1", "client": "bench-01", "model": "iq2_xs",
                  "since": "2026-10-04T08:00:00Z", "expected_until": "2026-10-04T16:00:00Z",
                  "note": "strata h2h run 3", "hold": "connection", "attached": false,
                  "last_activity": "2026-10-04T08:00:00Z", "idle_for": 7200,
                  "release_if_idle": null, "reclaimable": false },
                { "id": "L2", "client": "mac-sessions", "model": "iq2_xs",
                  "since": "2026-10-04T09:45:00Z", "expected_until": null,
                  "note": null, "hold": "heartbeat", "attached": true,
                  "last_activity": "2026-10-04T09:45:00Z", "idle_for": 900,
                  "release_if_idle": null, "reclaimable": false },
            ],
            "waiters": [
                { "client": "mac-sessions", "model": "qwen3.8:27b", "kind": "request",
                  "priority": "interactive", "since": "2026-10-04T09:59:00Z",
                  "reason": "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z, idle for 2h",
                  "reason_kind": "held",
                  "estimate": "2026-10-04T16:00:00Z" },
                { "client": "bench-01", "model": "iq3_s", "kind": "lease",
                  "priority": "batch", "since": "2026-10-04T09:59:30Z",
                  "reason": null, "reason_kind": null, "estimate": null },
            ],
            "errors": [
                { "model": "iq3_s", "at": "2026-10-04T07:00:00Z", "error": "out of memory" },
            ],
        })
    );
}

#[test]
fn a_declared_vram_of_all_reads_as_the_hosts_vram() {
    let clock = clock();
    let host = Host {
        vram: 25_757_220_864,
        ram: 66_520_760_320,
    };
    let mut all = snapshot(&clock);
    all.declared.vram = Vram::All;
    let mut none = snapshot(&clock);
    none.declared.vram = Vram::None;

    assert_eq!(
        status_body(&all, &host, &clock)["host"]["vram_declared_bytes"],
        json!(25_757_220_864_u64)
    );
    assert_eq!(
        status_body(&none, &host, &clock)["host"]["vram_declared_bytes"],
        json!(0)
    );
}

#[test]
fn v1_models_lists_only_models_a_client_can_ask_for() {
    let clock = clock();
    let config = config(TWO_MODELS);

    let body = models_body(&snapshot(&clock), &config);

    assert_eq!(
        body,
        json!({
            "object": "list",
            "data": [
                { "id": "iq2_xs", "object": "model", "created": 0, "owned_by": "paddock",
                  "loaded": true, "state": "loaded" },
                { "id": "laya", "object": "model", "created": 0, "owned_by": "paddock",
                  "loaded": false, "state": "unloaded" },
            ],
        })
    );
}

#[test]
fn api_tags_lists_only_models_on_ollamas_api() {
    let config = config(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx65536"
apis = ["openai", "ollama"]
vram = "19504M"
idle = "2h"

[models."gemma3:27b"]
backend = "ollama"
name = "gemma3:27b"
apis = ["openai"]
vram = "20G"
idle = "2h"
"#,
    );

    assert_eq!(
        tags_body(&config),
        json!({ "models": [{ "name": "qwen3.8:27b", "model": "qwen3.8:27b" }] })
    );
}

/// Two sheep models with no ready checks, and keys a status must never show.
const TWO_MODELS: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac-secret"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { TOKEN = "env-secret" } }
url = "http://127.0.0.1:8080"
vram = "all"
ram = "37G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
key = "k-laya-secret"
ram = "5G"
idle = "8h"
"#;

struct Endpoint {
    base: String,
    engine: EngineHandle,
    client: reqwest::Client,
}

impl Endpoint {
    async fn get(&self, path: &str, key: Option<&str>) -> (u16, String) {
        let mut request = self.client.get(format!("{}{path}", self.base));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let response = bounded("the response", request.send())
            .await
            .expect("the endpoint answers");
        let status = response.status().as_u16();
        let text = bounded("the body", response.text()).await.expect("a body");
        (status, text)
    }
}

async fn with_endpoint<F, Fut>(shepherd: FakeShepherd, body: F)
where
    F: FnOnce(Endpoint) -> Fut,
    Fut: Future<Output = ()>,
{
    let config = config(TWO_MODELS);
    let (engine, inbox) = channel();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (_reload, watched) = watch::channel(Arc::clone(&config));
    let shared = Shared {
        engine: engine.clone(),
        config: watched,
        http: crate::outbound::http_client(),
        timeouts: Timeouts::default(),
    };
    let backends = Backends::new(shepherd, crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(run(
        config,
        backends,
        Start::default(),
        inbox,
        Stop::never(),
    ));
    local.spawn_local(serve(listener, shared, Stop::never()));
    let endpoint = Endpoint {
        base: format!("http://{addr}"),
        engine,
        client: crate::outbound::http_client(),
    };
    local.run_until(body(endpoint)).await;
}

async fn until_loading(engine: &EngineHandle, model: &str) {
    let model = ModelName::from(model);
    bounded("the model loading", async {
        loop {
            let snapshot = engine.snapshot().await;
            if snapshot
                .models
                .iter()
                .any(|view| view.name == model && view.state == State::Loading)
            {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn v1_models_lists_every_model_with_its_state() {
    let shepherd = FakeShepherd::gated_restart();
    shepherd.open_gate();
    with_endpoint(shepherd, |endpoint| async move {
        let admitted = endpoint.engine.admit(
            ClientName::from("mac-sessions"),
            ModelName::from("laya"),
            Priority::Interactive,
            LIMIT,
        );
        drop(bounded("laya to load", admitted).await);
        let engine = endpoint.engine.clone();
        spawn_local(async move {
            let model = ModelName::from("iq2_xs");
            let client = ClientName::from("mac-sessions");
            let _ = engine
                .admit(client, model, Priority::Interactive, LIMIT)
                .await;
        });
        until_loading(&endpoint.engine, "iq2_xs").await;

        let (status, text) = endpoint.get("/v1/models", None).await;

        assert_eq!(status, 200);
        let body: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(
            body,
            json!({
                "object": "list",
                "data": [
                    { "id": "iq2_xs", "object": "model", "created": 0, "owned_by": "paddock",
                  "loaded": false, "state": "loading" },
                    { "id": "laya", "object": "model", "created": 0, "owned_by": "paddock",
                  "loaded": true, "state": "loaded" },
                ],
            })
        );
    })
    .await;
}

#[tokio::test]
async fn the_status_needs_a_key_and_shows_none() {
    with_endpoint(FakeShepherd::new(), |endpoint| async move {
        let (denied, _) = endpoint.get("/paddock/status", None).await;
        assert_eq!(denied, 401);

        let (status, text) = endpoint.get("/paddock/status", Some("k-mac-secret")).await;

        assert_eq!(status, 200);
        let body: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(body["host"]["vram_bytes"], json!(25_757_220_864_u64));
        assert_eq!(body["models"].as_array().map(Vec::len), Some(2));
        for secret in ["k-mac-secret", "k-laya-secret", "env-secret"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    })
    .await;
}
