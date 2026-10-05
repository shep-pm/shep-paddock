//! The lease routes over a real loopback socket with the engine behind them, so these tests run
//! on real time except the heartbeat one, which drives the stream body on a paused clock. Every
//! await is bounded by `LIMIT`.

use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use http_body_util::BodyExt;
use serde_json::{Value, json};
use shep_client::dogs::Stop;
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::LocalSet,
    time::{Instant, sleep, timeout},
};

mod hangups;
mod rejections;
mod units;

use super::{
    parse_id,
    stream::{LeaseStream, STREAM_HEARTBEAT},
};
use crate::{
    backend::Backends,
    book::LeaseId,
    config::{Config, ModelName},
    engine::{EngineHandle, LeaseEvent, channel, run},
    http::{Shared, Timeouts, serve},
    test_support::{FakeShepherd, config},
};

// Past any one step a test waits on: a load on the fake shepherd, one request, one line.
const LIMIT: Duration = Duration::from_secs(10);

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    match timeout(LIMIT, future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out: {what}"),
    }
}

/// Two clients and two models that each take all the VRAM, so one lease queues behind the other.
fn two_clients() -> Arc<Config> {
    config(
        r#"
reconnect = "60s"

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
backend = { sheep = "iq2_xs" }
url = "http://127.0.0.1:8080"
vram = "all"
idle = "2h"

[models.iq3_s]
backend = { sheep = "iq3_s" }
url = "http://127.0.0.1:8081"
vram = "all"
idle = "2h"
"#,
    )
}

struct Paddock {
    addr: SocketAddr,
    engine: EngineHandle,
    client: reqwest::Client,
}

impl Paddock {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
        body: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .request(method, format!("http://{}{path}", self.addr))
            .bearer_auth(key);
        if let Some(body) = body {
            request = request.body(body.to_owned());
        }
        bounded("the response head", request.send())
            .await
            .expect("the endpoint answers")
    }

    async fn take(&self, key: &str, body: &str) -> reqwest::Response {
        self.send(reqwest::Method::POST, "/paddock/leases", key, Some(body))
            .await
    }

    async fn status(&self, method: reqwest::Method, path: &str, key: &str) -> u16 {
        self.send(method, path, key, None).await.status().as_u16()
    }

    /// Takes a connection lease on `model` as `mac-sessions` and reads up to its grant.
    async fn held(&self, model: &str) -> (reqwest::Response, String) {
        let mut response = self
            .take("k-mac", &json!({ "model": model }).to_string())
            .await;
        let id = loop {
            let line = next_line(&mut response).await.expect("a line");
            if let Some(granted) = line.get("granted") {
                break granted["id"].as_str().expect("an id").to_owned();
            }
        };
        (response, id)
    }
}

async fn with_paddock<F, Fut>(shepherd: FakeShepherd, body: F)
where
    F: FnOnce(Paddock) -> Fut,
    Fut: Future<Output = ()>,
{
    let config = two_clients();
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
    local.spawn_local(run(config, backends, None, inbox, Stop::never()));
    local.spawn_local(serve(listener, shared, Stop::never()));
    let paddock = Paddock {
        addr,
        engine,
        client: crate::outbound::http_client(),
    };
    local.run_until(body(paddock)).await;
}

/// The next line of a streamed response, or `None` once its body has ended.
async fn next_line(response: &mut reqwest::Response) -> Option<Value> {
    // A stream line is one chunk here: the endpoint flushes each event as it writes it.
    let chunk = bounded("a stream line", response.chunk())
        .await
        .expect("the body")?;
    let text = std::str::from_utf8(&chunk).expect("UTF-8");
    assert!(text.ends_with('\n'), "a line ends in a newline: {text:?}");
    Some(serde_json::from_str(text.trim_end()).expect("one JSON object per line"))
}

async fn json_of(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = bounded("the body", response.bytes())
        .await
        .expect("the body");
    (status, serde_json::from_slice(&bytes).expect("a JSON body"))
}

async fn until_detached(engine: &EngineHandle) {
    bounded("the holder detaching", async {
        while !engine
            .snapshot()
            .await
            .leases
            .first()
            .is_some_and(|lease| !lease.attached)
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_lease_streams_queued_then_granted() {
    let shepherd = FakeShepherd::gated_restart();
    with_paddock(shepherd.clone(), |paddock| async move {
        let mut response = paddock.take("k-mac", r#"{"model":"iq2_xs"}"#).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "application/x-ndjson");

        let queued = next_line(&mut response).await.expect("a queued line");
        assert_eq!(queued["queued"]["reason"], "iq2_xs is loading", "{queued}");
        assert!(queued["queued"]["estimate"].is_string(), "{queued}");

        shepherd.open_gate();
        let granted = loop {
            let line = next_line(&mut response).await.expect("a line");
            if line.get("granted").is_some() {
                break line;
            }
            assert!(line.get("queued").is_some(), "{line}");
        };
        let id = granted["granted"]["id"].as_str().expect("an id");
        assert!(parse_id(id).is_some(), "{id}");
        assert_eq!(granted["granted"]["reconnect"], "60s");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_stream_heartbeats_every_fifteen_seconds() {
    let (events, rx) = mpsc::channel(4);
    let (_config, watched) = watch::channel(two_clients());
    let engine = channel().0;
    let mut stream = LeaseStream::new(rx, Some(ModelName::from("iq2_xs")), watched, engine.clock());
    events
        .send(LeaseEvent::Granted { lease: LeaseId(7) })
        .await
        .expect("send");
    let line = |frame: Option<Result<hyper::body::Frame<bytes::Bytes>, std::io::Error>>| {
        let data = frame.expect("a frame").expect("no error").into_data();
        serde_json::from_slice::<Value>(&data.expect("data")).expect("JSON")
    };

    let began = Instant::now();
    let granted = timeout(Duration::from_secs(30), stream.frame()).await;
    assert_eq!(
        line(granted.expect("a frame in time")),
        json!({"granted": {"id": "L7", "reconnect": "60s"}})
    );
    assert_eq!(began.elapsed(), Duration::ZERO);

    for beat in 1..=2 {
        // Past the interval, so a heartbeat that never came fails here rather than hangs.
        let heartbeat = timeout(Duration::from_secs(30), stream.frame())
            .await
            .expect("a heartbeat in time");
        assert_eq!(line(heartbeat), json!({"heartbeat": {}}));
        assert_eq!(began.elapsed(), STREAM_HEARTBEAT * beat);
    }
    assert_eq!(STREAM_HEARTBEAT, Duration::from_secs(15));
}

#[tokio::test]
async fn hanging_up_detaches_and_attach_resumes() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (response, id) = paddock.held("iq2_xs").await;

        drop(response);
        until_detached(&paddock.engine).await;

        let mut again = paddock
            .send(
                reqwest::Method::POST,
                &format!("/paddock/leases/{id}/attach"),
                "k-mac",
                None,
            )
            .await;
        assert_eq!(again.status(), 200);
        assert_eq!(again.headers()["content-type"], "application/x-ndjson");
        let granted = next_line(&mut again).await.expect("a granted line");
        assert_eq!(granted["granted"]["id"], id.as_str());
        assert_eq!(granted["granted"]["reconnect"], "60s");
        assert!(paddock.engine.snapshot().await.leases[0].attached);
    })
    .await;
}

#[tokio::test]
async fn attaching_twice_is_409() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_open, id) = paddock.held("iq2_xs").await;

        let (status, body) = json_of(
            paddock
                .send(
                    reqwest::Method::POST,
                    &format!("/paddock/leases/{id}/attach"),
                    "k-mac",
                    None,
                )
                .await,
        )
        .await;

        assert_eq!((status, body), (409, json!({"error": "attached"})));
    })
    .await;
}

#[tokio::test]
async fn another_clients_lease_is_403() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_open, id) = paddock.held("iq2_xs").await;
        let path = format!("/paddock/leases/{id}");

        for (method, path) in [
            (reqwest::Method::POST, format!("{path}/attach")),
            (reqwest::Method::PUT, path.clone()),
            (reqwest::Method::DELETE, path.clone()),
        ] {
            let (status, body) =
                json_of(paddock.send(method.clone(), &path, "k-bench", None).await).await;
            assert_eq!(
                (status, body),
                (403, json!({"error": "not_yours"})),
                "{method} {path}"
            );
        }
        assert_eq!(paddock.engine.snapshot().await.leases.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn a_heartbeat_lease_answers_once_granted() {
    let shepherd = FakeShepherd::gated_restart();
    with_paddock(shepherd.clone(), |paddock| async move {
        let body = r#"{"model":"iq2_xs","hold":"heartbeat","ttl":"30s"}"#;
        let waiting = tokio::task::spawn_local({
            let client = paddock.client.clone();
            let url = format!("http://{}/paddock/leases", paddock.addr);
            async move {
                client
                    .post(url)
                    .bearer_auth("k-mac")
                    .body(body)
                    .send()
                    .await
                    .expect("the endpoint answers")
            }
        });

        sleep(Duration::from_millis(200)).await;
        assert!(!waiting.is_finished(), "it answered before the grant");
        shepherd.open_gate();
        let response = bounded("the answer", waiting).await.expect("the task");

        let (status, body) = json_of(response).await;
        assert_eq!(status, 200);
        assert!(parse_id(body["id"].as_str().expect("an id")).is_some());
        assert_eq!(body["ttl"], "30s");
    })
    .await;
}

#[tokio::test]
async fn a_heartbeat_lease_without_a_ttl_gets_sixty_seconds() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (status, body) = json_of(
            paddock
                .take("k-mac", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                .await,
        )
        .await;

        assert_eq!((status, body["ttl"].clone()), (200, json!("60s")));
    })
    .await;
}

#[tokio::test]
async fn renew_and_release_answer_204() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_, body) = json_of(
            paddock
                .take("k-mac", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                .await,
        )
        .await;
        let path = format!("/paddock/leases/{}", body["id"].as_str().expect("an id"));

        assert_eq!(
            paddock.status(reqwest::Method::PUT, &path, "k-mac").await,
            204
        );
        assert_eq!(
            paddock
                .status(reqwest::Method::DELETE, &path, "k-mac")
                .await,
            204
        );
        assert_eq!(
            paddock
                .status(reqwest::Method::DELETE, &path, "k-mac")
                .await,
            404
        );
        assert_eq!(
            paddock.status(reqwest::Method::PUT, &path, "k-mac").await,
            404
        );
    })
    .await;
}

#[tokio::test]
async fn a_released_lease_ends_its_stream_with_released() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (mut response, id) = paddock.held("iq2_xs").await;

        let path = format!("/paddock/leases/{id}");
        assert_eq!(
            paddock
                .status(reqwest::Method::DELETE, &path, "k-mac")
                .await,
            204
        );

        let ended = next_line(&mut response).await.expect("an ended line");
        assert_eq!(ended, json!({"ended": {"why": "released"}}));
        assert_eq!(next_line(&mut response).await, None, "the body did not end");
    })
    .await;
}

#[tokio::test]
async fn a_refused_lease_ends_its_stream_with_the_busy_body() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_holder, _) = paddock.held("iq2_xs").await;

        let mut response = paddock
            .take("k-bench", r#"{"model":"iq3_s","max_wait":"50ms"}"#)
            .await;
        let refused = loop {
            let line = next_line(&mut response).await.expect("a line");
            if line.get("refused").is_some() {
                break line;
            }
        };

        assert_eq!(refused["refused"]["error"], "busy");
        assert_eq!(refused["refused"]["model"], "iq3_s");
        assert_eq!(next_line(&mut response).await, None, "the body did not end");
    })
    .await;
}

#[tokio::test]
async fn a_refused_heartbeat_lease_is_503() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_holder, _) = paddock.held("iq2_xs").await;

        let (status, body) = json_of(
            paddock
                .take(
                    "k-bench",
                    r#"{"model":"iq3_s","hold":"heartbeat","max_wait":"50ms"}"#,
                )
                .await,
        )
        .await;

        assert_eq!((status, body["error"].clone()), (503, json!("busy")));
    })
    .await;
}
