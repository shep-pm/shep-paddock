//! Fakes and fixtures shared by the unit tests.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    net::TcpListener as StdListener,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, service::service_fn};
use hyper_util::rt::TokioIo;
use shep_client::{EventStream, shep_core::protocol::ProcessInfo};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::{
    config::Config,
    shepherd::{Shepherd, ShepherdError},
};

/// Parses a config literal, for tests that need a `Config` and not its
/// validation. A bad literal is the test's bug, so it panics with the error.
pub(crate) fn config(toml: &str) -> Arc<Config> {
    match Config::from_toml(toml) {
        Ok(config) => Arc::new(config),
        Err(err) => panic!("test config does not parse: {err}"),
    }
}

/// The spec's example config with its real figures: the GPU host's totals,
/// the Strata models that each take all the VRAM, the ollama model beside
/// them, and laya, which holds only RAM. The book and engine tests build
/// their scenarios from it so they exercise the numbers the dog will run on.
pub(crate) const HOST_AND_MODELS: &str = r#"
listen = "0.0.0.0:8700"
grace = "2m"
max_wait = "120s"
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

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
apis = ["openai"]
vram = "22323M"
ram = "4G"
idle = "2h"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "37G"
idle = "2h"

[models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "44G"
idle = "2h"

[models.iq3_s]
backend = { sheep = "iq3_s" }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "55G"
excludes = ["laya"]
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "k-laya"
ready = { path = "/health", field = "loaded" }
ram = "5G"
idle = "8h"
"#;

/// One call the backends made of the shepherd, in the order made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    SetEnv(String, String, String),
    SetField(String, String, serde_json::Value),
    Restart(String),
    Stop(String),
}

/// A shepherd that records what it is asked, so a test can assert the exact order of calls
/// without a daemon, and refuses restarts on request. It answers nothing else with data: the
/// backends never read the flock or the dog section.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeShepherd {
    calls: Arc<Mutex<Vec<Call>>>,
    refuse_restart: Option<String>,
}

impl FakeShepherd {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Makes every restart answer `Refused` with `what`.
    pub(crate) fn refusing_restart(what: &str) -> Self {
        Self {
            refuse_restart: Some(what.to_owned()),
            ..Self::default()
        }
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record(&self, call: Call) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

impl Shepherd for FakeShepherd {
    async fn dog_config(&self, _name: &str) -> Result<String, ShepherdError> {
        Ok(String::new())
    }

    async fn list_flock(&self) -> Result<Vec<ProcessInfo>, ShepherdError> {
        Ok(Vec::new())
    }

    async fn set_field(
        &self,
        sheep: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), ShepherdError> {
        self.record(Call::SetField(sheep.to_owned(), key.to_owned(), value));
        Ok(())
    }

    async fn set_env(&self, sheep: &str, key: &str, value: &str) -> Result<(), ShepherdError> {
        self.record(Call::SetEnv(
            sheep.to_owned(),
            key.to_owned(),
            value.to_owned(),
        ));
        Ok(())
    }

    async fn restart(&self, sheep: &str) -> Result<(), ShepherdError> {
        self.record(Call::Restart(sheep.to_owned()));
        match &self.refuse_restart {
            Some(what) => Err(ShepherdError::Refused { what: what.clone() }),
            None => Ok(()),
        }
    }

    async fn stop(&self, sheep: &str) -> Result<(), ShepherdError> {
        self.record(Call::Stop(sheep.to_owned()));
        Ok(())
    }

    async fn process_events(&self) -> Result<EventStream, ShepherdError> {
        Err(ShepherdError::Unexpected {
            what: "no events from a fake",
        })
    }
}

/// One request the fake HTTP server saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

/// `(method, path, answers)`: each hit takes the next `(status, body)`, and the last repeats.
pub(crate) type Route = (&'static str, &'static str, Vec<(u16, &'static str)>);

/// The running fake server. Dropping it stops the server.
#[derive(Debug)]
pub(crate) struct FakeHttp {
    seen: Arc<Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}

impl FakeHttp {
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen lock").clone()
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A tiny hyper server on `127.0.0.1:0` that answers each route from its script, and 404 for
/// anything else, so a test sees the exact requests the backends sent. It binds a real
/// loopback socket, so tests that use it run on real time: a paused clock auto-advances while
/// the socket is still in flight and fires the test's own timeouts early. Returns the base url.
pub(crate) fn fake_http(routes: Vec<Route>) -> (String, FakeHttp) {
    let std_listener = StdListener::bind("127.0.0.1:0").expect("bind loopback");
    std_listener.set_nonblocking(true).expect("non-blocking");
    let base = format!("http://{}", std_listener.local_addr().expect("local addr"));
    let listener = TcpListener::from_std(std_listener).expect("tokio listener");
    let scripts: HashMap<(String, String), VecDeque<(u16, String)>> = routes
        .into_iter()
        .map(|(method, path, answers)| {
            let answers = answers
                .into_iter()
                .map(|(s, b)| (s, b.to_owned()))
                .collect();
            ((method.to_owned(), path.to_owned()), answers)
        })
        .collect();
    let scripts = Arc::new(Mutex::new(scripts));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let seen = Arc::clone(&seen);
        async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let scripts = Arc::clone(&scripts);
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let scripts = Arc::clone(&scripts);
                        let seen = Arc::clone(&seen);
                        async move { Ok::<_, Infallible>(answer(req, &scripts, &seen).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        }
    });
    (base, FakeHttp { seen, task })
}

type Scripts = Mutex<HashMap<(String, String), VecDeque<(u16, String)>>>;

async fn answer(
    req: Request<hyper::body::Incoming>,
    scripts: &Scripts,
    seen: &Mutex<Vec<Seen>>,
) -> Response<Full<Bytes>> {
    let (parts, body) = req.into_parts();
    let body = match body.collect().await {
        Ok(collected) => String::from_utf8_lossy(&collected.to_bytes()).into_owned(),
        Err(_) => String::new(),
    };
    let key = (parts.method.to_string(), parts.uri.path().to_owned());
    seen.lock().expect("seen lock").push(Seen {
        method: key.0.clone(),
        path: key.1.clone(),
        authorization: parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body,
    });
    let (status, body) = {
        let mut scripts = scripts.lock().expect("scripts lock");
        match scripts.get_mut(&key) {
            Some(queue) if queue.len() > 1 => queue.pop_front().expect("non-empty"),
            Some(queue) => queue.front().cloned().unwrap_or((404, String::new())),
            None => (404, String::new()),
        }
    };
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(body)))
        .expect("response")
}

/// One model from [`HOST_AND_MODELS`], cloned out so a test can point its url at a fake server.
pub(crate) fn model(name: &str) -> crate::config::Model {
    let config = config(HOST_AND_MODELS);
    match config.models.get(&crate::config::ModelName::from(name)) {
        Some(model) => model.clone(),
        None => panic!("{name} is not in HOST_AND_MODELS"),
    }
}
