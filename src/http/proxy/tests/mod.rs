//! The proxy end to end: a client, the endpoint, the engine on a fake shepherd, and fake backends.
//! Every backend is a real loopback socket, so these tests run on real time, and every await is
//! bounded by `LIMIT`.

use std::{
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use http_body_util::StreamBody;
use hyper::{Response, body::Frame, service::service_fn};
use hyper_util::rt::TokioIo;
use reqwest::Url;
use serde_json::{Value, json};
use shep_client::dogs::Stop;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::LocalSet,
    time::{sleep, timeout},
};

use super::{BadRequest, MAX_BODY, asks_to_unload, read_body, target, to_backend};
use crate::{
    backend::Backends,
    book::{Hold, Leased, Priority},
    config::{Config, ModelName},
    engine::{EngineHandle, LeaseEvent, LeaseRequest, Start, channel, run},
    http::{Shared, Timeouts, serve},
    test_support::{Call, FakeShepherd, config, fake_http},
};

mod bodies;
mod forward;
mod paths;
mod refused;

// Past any one step a test waits on: a load on the fake shepherd, one request, one chunk.
const LIMIT: Duration = Duration::from_secs(10);

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    match timeout(LIMIT, future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out: {what}"),
    }
}

/// The host and the one client every scenario shares, with the test's own models.
fn paddock_config(models: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"
{models}"#
    ))
}

/// The endpoint and the engine behind it, running beside the test
struct Paddock {
    addr: SocketAddr,
    engine: EngineHandle,
    client: reqwest::Client,
}

impl Paddock {
    /// Posts `body` to `path` as `mac-sessions`, with `headers` added.
    async fn post(&self, path: &str, body: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = self
            .client
            .post(format!("http://{}{path}", self.addr))
            .bearer_auth("k-mac")
            .body(body.to_owned());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        bounded("the response head", request.send())
            .await
            .expect("the endpoint answers")
    }

    async fn in_flight(&self, model: &str) -> Option<u32> {
        let model = ModelName::from(model);
        let snapshot = self.engine.snapshot().await;
        let view = snapshot.models.iter().find(|view| view.name == model)?;
        Some(view.in_flight)
    }

    /// Waits for `model`'s in-flight count to reach `count`, failing the test if it does not.
    async fn until_in_flight(&self, model: &str, count: u32) {
        bounded(&format!("{model} reaching {count} in flight"), async {
            while self.in_flight(model).await != Some(count) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}

/// Runs `body` against an endpoint whose engine drives `shepherd`, all on this test's thread.
async fn with_paddock<F, Fut>(config: Arc<Config>, shepherd: FakeShepherd, body: F)
where
    F: FnOnce(Paddock) -> Fut,
    Fut: Future<Output = ()>,
{
    with_paddock_timed(config, shepherd, Timeouts::default(), body).await;
}

/// [`with_paddock`], with the endpoint giving clients `timeouts`.
async fn with_paddock_timed<F, Fut>(
    config: Arc<Config>,
    shepherd: FakeShepherd,
    timeouts: Timeouts,
    body: F,
) where
    F: FnOnce(Paddock) -> Fut,
    Fut: Future<Output = ()>,
{
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
        timeouts,
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
    let paddock = Paddock {
        addr,
        engine,
        client: crate::outbound::http_client(),
    };
    local.run_until(body(paddock)).await;
}

/// A backend that answers every request with a 302 to `location`.
async fn fake_redirect(location: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    let location = location.to_owned();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let location = location.clone();
            let service = service_fn(move |_request| {
                let response = Response::builder()
                    .status(302)
                    .header("location", location.as_str())
                    .body(http_body_util::Empty::<Bytes>::new());
                async move { response }
            });
            tokio::spawn(
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service),
            );
        }
    });
    base
}

/// A backend that answers each request with an event stream the test feeds chunk by chunk, so
/// streaming shows apart from buffering. Dropping the sender ends the stream.
async fn fake_sse() -> (String, mpsc::UnboundedSender<Bytes>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    let feed = Arc::new(Mutex::new(Some(rx)));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let feed = Arc::clone(&feed);
            let service = service_fn(move |_request| {
                let rx = feed.lock().expect("feed lock").take();
                let chunks = stream::unfold(rx, |rx| async move {
                    let mut rx = rx?;
                    let chunk = rx.recv().await?;
                    Some((Ok::<_, Infallible>(Frame::data(chunk)), Some(rx)))
                });
                let response = Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(StreamBody::new(chunks));
                async move { response }
            });
            tokio::spawn(
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service),
            );
        }
    });
    (base, tx)
}

async fn json_of(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = bounded("the body", response.bytes())
        .await
        .expect("the body");
    (status, serde_json::from_slice(&bytes).expect("a JSON body"))
}

async fn text_of(response: reqwest::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let text = bounded("the body", response.text())
        .await
        .expect("the body");
    (status, text)
}

/// One sheep model with no ready check at `url`, so it is loaded once its restart answers.
fn sheep(name: &str, url: &str, extra: &str) -> String {
    format!(
        r#"
[models.{name}]
backend = {{ sheep = "{name}" }}
url = "{url}"
vram = "all"
idle = "2h"
{extra}
"#
    )
}

/// Sends a request for `iq2_xs` with `body`, and asserts the answer came before any load or forward.
async fn refused_at_once(body: &str, headers: &[(&str, &str)]) -> (u16, Value) {
    let (base, server) = fake_http(vec![("POST", "/v1/chat/completions", vec![(200, "{}")])]);
    let shepherd = FakeShepherd::new();
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    let mut answer = None;
    let slot = &mut answer;
    with_paddock(config, shepherd.clone(), |paddock| async move {
        let response = paddock.post("/v1/chat/completions", body, headers).await;
        *slot = Some(json_of(response).await);
    })
    .await;
    assert!(server.seen().is_empty(), "the request was forwarded");
    assert!(shepherd.calls().is_empty(), "the model was loaded");
    answer.expect("answered")
}

/// laya on `base`, routed by `prefix` as given, past the config's own check on prefixes.
fn laya_with_prefix(base: &str, prefix: &str) -> Arc<Config> {
    let models = format!(
        r#"
[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
prefix = "/laya"
key = "k-laya"
ram = "5G"
idle = "8h"
"#
    );
    let mut config = (*paddock_config(&models)).clone();
    let laya = config
        .models
        .get_mut(&ModelName::from("laya"))
        .expect("laya is configured");
    laya.prefix = Some(prefix.to_owned());
    Arc::new(config)
}

async fn get(paddock: &Paddock, path: &str) -> reqwest::Response {
    let request = paddock
        .client
        .get(format!("http://{}{path}", paddock.addr))
        .bearer_auth("k-mac")
        .send();
    bounded("a GET", request)
        .await
        .expect("the endpoint answers")
}

fn chunked(
    sizes: &[usize],
) -> StreamBody<impl futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>>> {
    let frames: Vec<_> = sizes
        .iter()
        .map(|size| Ok(Frame::data(Bytes::from(vec![b'x'; *size]))))
        .collect();
    StreamBody::new(stream::iter(frames))
}

#[tokio::test]
async fn a_body_with_no_length_is_cut_off_past_the_cap() {
    let half = MAX_BODY / 2;

    assert_eq!(
        read_body(chunked(&[half, half]), LIMIT)
            .await
            .map(|body| body.len()),
        Ok(MAX_BODY)
    );
    assert_eq!(
        read_body(chunked(&[half, half, 1]), LIMIT).await,
        Err(BadRequest::TooLarge)
    );
}

#[tokio::test(start_paused = true)]
async fn a_body_that_stalls_is_too_slow_after_the_body_timeout() {
    let stalled = StreamBody::new(stream::pending::<Result<Frame<Bytes>, Infallible>>());
    let began = tokio::time::Instant::now();

    // The outer bound fails the test if the body timeout never fires.
    let read = timeout(
        Duration::from_secs(120),
        read_body(stalled, Duration::from_secs(60)),
    )
    .await
    .expect("read_body gave up");

    assert_eq!(read, Err(BadRequest::TooSlow));
    assert_eq!(began.elapsed(), Duration::from_secs(60));
}

#[test]
fn hop_by_hop_client_keys_and_the_dogs_own_headers_stay_behind() {
    let mut headers = hyper::HeaderMap::new();
    for name in [
        "connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "proxy-authorization",
        "authorization",
        "x-api-key",
        "cookie",
        "host",
        "content-length",
        "x-paddock-priority",
        "x-paddock-max-wait",
        "content-type",
        "accept",
    ] {
        headers.insert(name, hyper::header::HeaderValue::from_static("v"));
    }

    let kept: Vec<_> = to_backend(&headers)
        .keys()
        .map(|name| name.as_str().to_owned())
        .collect();

    assert_eq!(kept, ["content-type", "accept"]);
}

#[test]
fn headers_a_clients_connection_names_stay_behind() {
    let mut headers = hyper::HeaderMap::new();
    let value = hyper::header::HeaderValue::from_static;
    headers.append("connection", value("X-Trace , close"));
    headers.append("connection", value("x-session"));
    for name in ["x-trace", "x-session", "close", "x-kept"] {
        headers.insert(name, value("v"));
    }

    let kept: Vec<_> = to_backend(&headers)
        .keys()
        .map(|name| name.as_str().to_owned())
        .collect();

    assert_eq!(kept, ["x-kept"]);
}

#[test]
fn the_target_keeps_the_base_scheme_host_and_port() {
    let at = |base: &str, path: &str, query: Option<&str>| {
        let base = Url::parse(base).ok()?;
        target(&base, path, query).map(|url| url.to_string())
    };

    assert_eq!(
        at("http://127.0.0.1:8000", "/@evil.com/x", None).as_deref(),
        Some("http://127.0.0.1:8000/@evil.com/x")
    );
    assert_eq!(
        at("http://127.0.0.1:8000", "@evil.com/x", None).as_deref(),
        Some("http://127.0.0.1:8000/@evil.com/x")
    );
    assert_eq!(
        at("http://127.0.0.1:8000/", "//evil.com/x", Some("a=1#f")).as_deref(),
        Some("http://127.0.0.1:8000//evil.com/x?a=1%23f")
    );
    assert_eq!(
        at("http://127.0.0.1:8000/api/", "/v1/x", None).as_deref(),
        Some("http://127.0.0.1:8000/api/v1/x")
    );
    assert_eq!(at("unix:/run/laya.sock", "/v1/x", None), None);
}
