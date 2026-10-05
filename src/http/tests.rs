//! The endpoint over a real loopback socket, so these tests run on real time.
//! Every await is bounded by `LIMIT`.

use std::{future::Future, time::Duration};

use http_body_util::BodyExt;
use hyper::{StatusCode, body::Body as _, header::RETRY_AFTER};
use shep_client::dogs::Stop;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};

use super::{Body, DRAIN, Shared, Timeouts, reply, serve};
use crate::{
    book::{Reason, Refusal},
    config::{ClientName, ModelName},
    engine::{Clock, channel},
    test_support::{HOST_AND_MODELS, config},
};

const LIMIT: Duration = Duration::from_secs(5);

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    match timeout(LIMIT, future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out: {what}"),
    }
}

struct Served {
    addr: std::net::SocketAddr,
    stop: shep_client::dogs::StopRequest,
    task: tokio::task::JoinHandle<()>,
    _inbox: crate::engine::Inbox,
    _reload: watch::Sender<std::sync::Arc<crate::config::Config>>,
}

async fn start() -> Served {
    start_with(Timeouts::default()).await
}

async fn start_with(timeouts: Timeouts) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (engine, inbox) = channel();
    let (reload, config) = watch::channel(config(HOST_AND_MODELS));
    let state = Shared {
        engine,
        config,
        http: crate::outbound::http_client(),
        timeouts,
    };
    let (stop, request) = Stop::new();
    let task = tokio::spawn(serve(listener, state, stop));
    Served {
        addr,
        stop: request,
        task,
        _inbox: inbox,
        _reload: reload,
    }
}

/// Sends one request and returns its status and body.
async fn get(addr: std::net::SocketAddr, path: &str, authorization: Option<&str>) -> (u16, String) {
    bounded("one request", async {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let auth = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let request = format!("GET {path} HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut text = String::new();
        stream.read_to_string(&mut text).await.expect("read");
        let status = text[9..12].parse().expect("status");
        let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
        (status, body)
    })
    .await
}

#[tokio::test]
async fn a_missing_key_is_401() {
    let served = start().await;

    let (status, body) = get(served.addr, "/v1/chat/completions", None).await;

    assert_eq!(
        (status, body.as_str()),
        (401, r#"{"error":"unauthorized"}"#)
    );
}

#[tokio::test]
async fn a_wrong_key_is_401() {
    let served = start().await;

    for header in ["Bearer nope", "Bearer ", "Basic k-mac", "k-mac"] {
        let (status, body) = get(served.addr, "/anything", Some(header)).await;
        assert_eq!(
            (status, body.as_str()),
            (401, r#"{"error":"unauthorized"}"#),
            "{header}"
        );
    }
}

#[tokio::test]
async fn v1_models_needs_no_key() {
    let served = start().await;

    let (status, _) = get(served.addr, "/v1/models", None).await;

    assert_ne!(status, 401);
}

#[tokio::test]
async fn an_unknown_route_is_404() {
    let served = start().await;

    let (status, body) = get(served.addr, "/nowhere", Some("Bearer k-mac")).await;

    assert_eq!((status, body.as_str()), (404, r#"{"error":"not_found"}"#));
}

#[tokio::test]
async fn serve_returns_once_stopped() {
    let served = start().await;

    served.stop.request();

    bounded("serve to return", served.task)
        .await
        .expect("serve did not panic");
}

/// Real time with a short timeout: a paused clock races the socket, since tokio can
/// advance it past the bound before the server has armed its timer.
#[tokio::test]
async fn a_client_that_stalls_mid_head_is_disconnected() {
    let header_read = Duration::from_millis(200);
    let served = start_with(Timeouts {
        header_read,
        ..Timeouts::default()
    })
    .await;
    // Taken before connecting, so the server's timer cannot start earlier.
    let began = tokio::time::Instant::now();
    let mut stream = TcpStream::connect(served.addr).await.expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("write");

    let mut rest = Vec::new();
    bounded(
        "the server closing the connection",
        stream.read_to_end(&mut rest),
    )
    .await
    .expect("read");

    assert!(began.elapsed() >= header_read);
}

#[tokio::test(start_paused = true)]
async fn serve_returns_within_the_drain_with_a_connection_open() {
    let served = start().await;
    let mut stream = TcpStream::connect(served.addr).await.expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("write");
    // Let the server accept it, so the connection is open when the stop comes.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let began = tokio::time::Instant::now();

    served.stop.request();
    timeout(DRAIN * 2, served.task)
        .await
        .expect("serve returned")
        .expect("serve did not panic");

    assert!(began.elapsed() <= DRAIN);
    drop(stream);
}

async fn body_of(response: hyper::Response<Body>) -> serde_json::Value {
    assert!(response.body().size_hint().exact().is_some());
    let bytes = bounded("body", response.into_body().collect())
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}

fn held(clock: &Clock, until: Option<&str>) -> Reason {
    let at = |text: &str| clock.moment_of(text.parse().expect("timestamp"));
    Reason::Held {
        model: "iq2_xs".into(),
        client: ClientName::from("bench-01"),
        lease: crate::book::LeaseId(1),
        since: at("2026-10-04T08:00:00Z"),
        until: until.map(at),
    }
}

#[tokio::test]
async fn busy_has_retry_after_when_there_is_an_estimate() {
    let clock = Clock::new();
    let refusal = Refusal {
        reason: held(&clock, Some("2026-10-04T20:00:00Z")),
        retry_after: Some(Duration::from_millis(1500)),
    };

    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[RETRY_AFTER], "2");
    assert_eq!(
        body_of(response).await,
        serde_json::json!({
            "error": "busy",
            "model": "qwen3.8:27b",
            "reason": "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z",
            "expected_until": "2026-10-04T20:00:00Z",
        })
    );
}

#[tokio::test]
async fn busy_has_no_retry_after_without_one() {
    let clock = Clock::new();
    let refusal = Refusal {
        reason: held(&clock, None),
        retry_after: None,
    };

    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get(RETRY_AFTER).is_none());
    assert_eq!(
        body_of(response).await["expected_until"],
        serde_json::Value::Null
    );
}

#[test]
fn each_reason_reads_as_a_sentence() {
    let clock = Clock::new();
    let at = clock.moment_of("2026-10-04T09:30:00Z".parse().expect("timestamp"));
    let model = |name: &str| ModelName::from(name);
    let say = |reason| reply::sentence(&reason, &clock);

    assert_eq!(
        say(Reason::Loading {
            model: model("iq2_xs")
        }),
        "iq2_xs is loading"
    );
    assert_eq!(
        say(Reason::Evicting {
            model: model("laya"),
            for_model: model("iq3_s")
        }),
        "laya is evicting for iq3_s"
    );
    assert_eq!(
        say(Reason::Draining {
            model: model("laya")
        }),
        "laya is unloading"
    );
    assert_eq!(
        say(Reason::Grace {
            model: model("iq2_xs"),
            until: at
        }),
        "iq2_xs is in its grace period until 2026-10-04T09:30:00Z"
    );
    assert_eq!(
        say(held(&clock, None)),
        "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z"
    );
    assert_eq!(
        say(Reason::Behind {
            model: model("iq3_s")
        }),
        "iq3_s is loading or claimed by another waiter"
    );
}
