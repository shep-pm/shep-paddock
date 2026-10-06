//! The endpoint over a real loopback socket, so these tests run on real time.
//! Every await is bounded by `LIMIT`.

use std::{future::Future, time::Duration};

use http_body_util::BodyExt;
use hyper::{
    HeaderMap, StatusCode,
    body::Body as _,
    header::{AUTHORIZATION, RETRY_AFTER},
};
use shep_client::dogs::Stop;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};

use super::{Body, DRAIN, Shared, Timeouts, authenticate, hung_up, reply, serve, serve_draining};
use crate::{
    book::{Reason, Refusal},
    config::{Client, ClientName, ModelName},
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
        let status = text
            .get(9..12)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in the answer: {text:?}"));
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

#[test]
fn a_bearer_header_with_no_token_is_refused_before_any_key_is_compared() {
    let mut config = config(HOST_AND_MODELS);
    // Config loading refuses an empty key, so this stands for a client the
    // empty-token filter alone keeps out.
    std::sync::Arc::make_mut(&mut config)
        .clients
        .push(Client::with_key(ClientName::from("blank"), ""));
    let headers = |value: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value.parse().expect("header value"));
        headers
    };

    let denied = authenticate(&config, &headers("Bearer ")).expect_err("no token");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        authenticate(&config, &headers("Bearer k-mac"))
            .expect("a valid key")
            .name
            .as_str(),
        "mac-sessions"
    );
}

#[test]
fn the_bearer_scheme_matches_in_any_case() {
    let config = config(HOST_AND_MODELS);
    for value in ["bearer k-mac", "BEARER k-mac", "bEaReR k-mac"] {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value.parse().expect("header value"));

        let client = authenticate(&config, &headers).expect(value);

        assert_eq!(client.name.as_str(), "mac-sessions", "{value}");
    }
}

#[tokio::test]
async fn v1_models_needs_no_key() {
    let served = start().await;
    // No engine runs here, so a stopped one answers the snapshot at once.
    drop(served._inbox);

    let (status, body) = get(served.addr, "/v1/models", None).await;

    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn api_tags_needs_no_key() {
    let served = start().await;

    let (status, body) = get(served.addr, "/api/tags", None).await;

    assert_eq!((status, body.as_str()), (200, r#"{"models":[]}"#));
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

/// Serves one connection with a trivial service while `client` writes `sent` and then
/// closes or resets, and returns the error serving it ended in.
async fn served_error(sent: &'static [u8], reset: bool) -> hyper::Error {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let mut client = TcpStream::connect(listener.local_addr().expect("addr"))
        .await
        .expect("connect");
    let (server, _) = listener.accept().await.expect("accept");
    client.write_all(sent).await.expect("write");
    if reset {
        client.set_zero_linger().expect("linger");
    }
    drop(client);
    let service = hyper::service::service_fn(|_request| async {
        Ok::<_, std::convert::Infallible>(hyper::Response::new(
            http_body_util::Empty::<bytes::Bytes>::new(),
        ))
    });
    let served = hyper::server::conn::http1::Builder::new()
        .serve_connection(hyper_util::rt::TokioIo::new(server), service);
    bounded("the connection to end", served)
        .await
        .expect_err("the connection ended badly")
}

#[tokio::test]
async fn a_client_that_hangs_up_or_resets_mid_request_has_only_hung_up() {
    let closed = served_error(b"GET / HTTP/1.1\r\nHost: x\r\n", false).await;
    let reset = served_error(b"GET / HTTP/1.1\r\nHost: x\r\n", true).await;
    let garbled = served_error(b"NOT HTTP AT ALL\r\n\r\n", false).await;

    assert!(hung_up(&closed), "{closed:?}");
    assert!(hung_up(&reset), "{reset:?}");
    assert!(!hung_up(&garbled), "{garbled:?}");
}

/// A request the engine never answers holds its connection past a graceful stop, so only
/// the drain's end closes it. Real time with a short drain, as there is a real socket.
#[tokio::test]
async fn a_connection_still_busy_when_the_drain_ends_is_aborted() {
    let drain = Duration::from_millis(300);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (engine, _inbox) = channel();
    let watching = engine.clone();
    let (_reload, config) = watch::channel(config(HOST_AND_MODELS));
    let state = Shared {
        engine,
        config,
        http: crate::outbound::http_client(),
        timeouts: Timeouts::default(),
    };
    let (stop, request) = Stop::new();
    let task = tokio::spawn(serve_draining(listener, state, stop, drain));
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /laya/health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\n\r\n")
        .await
        .expect("write");
    bounded("the request reaching the engine's inbox", async {
        while watching.queued() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let began = tokio::time::Instant::now();

    request.request();
    bounded("serve to return", task)
        .await
        .expect("serve did not panic");

    assert!(began.elapsed() >= drain, "returned before the drain ended");
    let mut rest = Vec::new();
    bounded("the connection closing", stream.read_to_end(&mut rest))
        .await
        .expect("read");
    assert!(rest.is_empty(), "{rest:?}");
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
        idle_since: None,
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

/// `expected_until` for a refusal that is not a hold is the moment of the reply plus its retry.
async fn expected_until_of(reason: Reason, retry_after: Option<Duration>) -> serde_json::Value {
    let clock = Clock::new();
    let refusal = Refusal {
        reason,
        retry_after,
    };
    let before = clock.wall(clock.moment());
    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);
    let after = clock.wall(clock.moment());
    let expected = body_of(response).await["expected_until"].clone();
    if let Some(retry) = retry_after {
        let at: jiff::Timestamp = expected
            .as_str()
            .expect("a string")
            .parse()
            .expect("rfc 3339");
        let retry = jiff::SignedDuration::try_from(retry).expect("duration");
        assert!(at >= before.checked_add(retry).expect("add"), "{at}");
        assert!(at <= after.checked_add(retry).expect("add"), "{at}");
    }
    expected
}

#[tokio::test]
async fn a_grace_refusal_expects_the_retry_after_from_now() {
    let clock = Clock::new();
    let reason = Reason::Grace {
        model: "iq2_xs".into(),
        until: clock.moment(),
    };
    expected_until_of(reason.clone(), Some(Duration::from_secs(90))).await;
    assert_eq!(
        expected_until_of(reason, None).await,
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn a_loading_refusal_expects_the_retry_after_from_now() {
    let reason = Reason::Loading {
        model: "iq2_xs".into(),
    };
    expected_until_of(reason.clone(), Some(Duration::from_secs(90))).await;
    assert_eq!(
        expected_until_of(reason, None).await,
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
