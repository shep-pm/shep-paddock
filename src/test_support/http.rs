//! The fake HTTP server the backend and CLI tests talk to.

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
use tokio::{net::TcpListener, task::JoinHandle};

/// One request the fake HTTP server saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
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
        query: parts.uri.query().map(str::to_owned),
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
