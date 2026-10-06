//! The endpoint: an HTTP/1.1 server that authenticates each client and routes each request.

use std::{convert::Infallible, sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode, body::Incoming, header::AUTHORIZATION,
    server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use shep_client::dogs::Stop;
use tokio::{
    net::TcpListener,
    sync::watch,
    task::JoinSet,
    time::{sleep, timeout},
};

use crate::{
    config::{Client, Config},
    engine::EngineHandle,
};

mod lease;
mod proxy;
pub(crate) mod reply;
mod status;

#[cfg(test)]
mod tests;

/// How long open connections get to finish once a stop is requested
const DRAIN: Duration = Duration::from_secs(5);

// A client gets this long to send a request head. Heads are a few hundred
// bytes, so a slower sender is idle or hostile; 10 s is hyper's own default.
const HEADER_READ: Duration = Duration::from_secs(10);

// A client gets this long to send a request body. The largest, 32 MiB,
// takes about 3 s at 100 Mbit/s, so this leaves a slow link room.
const BODY_READ: Duration = Duration::from_secs(60);

// An accept that fails, such as on a full fd table, tends to keep failing, so
// the loop waits rather than spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A response body, buffered or streamed
pub(crate) type Body = BoxBody<Bytes, std::io::Error>;

/// How long a client gets to send each part of a request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeouts {
    /// For the request head.
    pub header_read: Duration,
    /// For the request body.
    pub body_read: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            header_read: HEADER_READ,
            body_read: BODY_READ,
        }
    }
}

/// What every connection's handler shares
#[derive(Debug, Clone)]
pub(crate) struct Shared {
    /// The engine that decides admission.
    pub engine: EngineHandle,
    /// The current config; a reload replaces it.
    pub config: watch::Receiver<Arc<Config>>,
    /// Forwards requests to the backends.
    pub http: reqwest::Client,
    /// How long a client gets to send a request.
    pub timeouts: Timeouts,
}

/// Serves `listener` until a stop is requested, then lets open connections finish for a few seconds
pub(crate) async fn serve(listener: TcpListener, state: Shared, stop: Stop) {
    serve_draining(listener, state, stop, DRAIN).await;
}

/// [`serve`] with `drain` as the time open connections get once a stop is requested
async fn serve_draining(listener: TcpListener, state: Shared, mut stop: Stop, drain: Duration) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.wait() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(stream, state.clone(), stop.clone()));
                }
                Err(err) => {
                    eprintln!("paddock: accepting a connection failed: {err}");
                    // A stop still ends the wait.
                    tokio::select! {
                        () = stop.wait() => break,
                        () = sleep(ACCEPT_BACKOFF) => {}
                    }
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    let drained = timeout(drain, async {
        while connections.join_next().await.is_some() {}
    });
    if drained.await.is_err() {
        connections.abort_all();
    }
}

async fn connection(stream: tokio::net::TcpStream, state: Shared, mut stop: Stop) {
    let header_read = state.timeouts.header_read;
    let service = service_fn(move |request| {
        let state = state.clone();
        async move { Ok::<_, Infallible>(route(&state, request).await) }
    });
    let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read)
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(served);
    let result = tokio::select! {
        result = served.as_mut() => result,
        () = stop.wait() => {
            served.as_mut().graceful_shutdown();
            served.await
        }
    };
    if let Err(err) = result {
        eprintln!("paddock: a connection ended badly: {err}");
    }
}

/// The client whose key the request carries
///
/// The `Bearer` scheme matches in any case, as RFC 7235 says. A missing
/// header, another scheme, an empty token and a wrong key all give the
/// same reply, so it does not say which was wrong.
///
/// # Errors
/// The `401` to send back when the request does not carry a client's key.
// The reply is sent at once, so boxing it would only add an allocation.
#[allow(clippy::result_large_err)]
pub(crate) fn authenticate<'c>(
    config: &'c Config,
    headers: &HeaderMap,
) -> Result<&'c Client, Response<Body>> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| {
            let (scheme, token) = value.as_bytes().split_at_checked(b"Bearer ".len())?;
            scheme.eq_ignore_ascii_case(b"Bearer ").then_some(token)
        })
        .filter(|token| !token.is_empty())
        .and_then(|token| config.client_for_key(token))
        .ok_or_else(|| reply::error(StatusCode::UNAUTHORIZED, "unauthorized"))
}

async fn route(state: &Shared, request: Request<Incoming>) -> Response<Body> {
    let config = Arc::clone(&state.config.borrow());
    let caller = authenticate(&config, request.headers());
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/v1/models") => status::models(state, &config).await,
        (&Method::GET, "/api/tags") => status::tags(&config),
        (method, path) => match caller {
            Err(denied) => denied,
            Ok(_) if method == Method::GET && path == "/paddock/status" => {
                status::status(state, &config).await
            }
            Ok(client) if lease::is_route(request.uri().path()) => {
                lease::handle(state, client, request).await
            }
            Ok(client) => proxy::proxy(state, client, request).await,
        },
    }
}
