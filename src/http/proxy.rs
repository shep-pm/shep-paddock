//! Requests for a model: routed, admitted, forwarded, and streamed back.

use core::fmt;
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures_util::StreamExt as _;
use http_body_util::{BodyExt, LengthLimitError, Limited, StreamBody};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode, Uri,
    body::{Frame, Incoming, SizeHint},
    header::{AUTHORIZATION, CONTENT_LENGTH, HOST, HeaderName, HeaderValue},
    http::request::Parts,
};
use reqwest::Url;
use serde_json::{Value, json};
use shep_client::shep_core::values::UpDuration;
use tokio::time::timeout;

use super::{Body, Shared, reply};
use crate::{
    book::Priority,
    config::{Api, Backend, Client, Config, Model, ModelName},
    engine::{Admission, InFlight},
};

#[cfg(test)]
mod tests;

/// The most of a request body the dog reads
// A 256K-token prompt is about 1 MiB of JSON at four bytes a token, so
// 32 MiB leaves room for images and long tool results.
const MAX_BODY: usize = 32 * 1024 * 1024;

// The Anthropic API's own key header, which a client may send in place of Authorization.
const API_KEY: &str = "x-api-key";
const PRIORITY: &str = "x-paddock-priority";
const MAX_WAIT: &str = "x-paddock-max-wait";
const PRIORITIES: [&str; 2] = ["interactive", "batch"];

/// Headers about one connection rather than the message, which never cross the proxy
const HOP_BY_HOP: [&str; 6] = [
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
];

/// Why a request was answered before the engine saw it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BadRequest {
    /// The body is longer than [`MAX_BODY`], by its declared length or as read.
    TooLarge,
    /// The body ended in a transport error before it was read in full.
    Unreadable,
    /// An API route's body does not parse as JSON.
    NotJson,
    /// An API route's body has no string `model` at its top level.
    NoModel,
    /// `X-Paddock-Max-Wait` is not a duration in shep's `UpDuration` grammar.
    MaxWait,
    /// The body did not arrive in full within the body timeout.
    TooSlow,
    /// `X-Paddock-Priority` is neither `interactive` nor `batch`.
    Priority,
}

impl fmt::Display for BadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "body_too_large",
            Self::Unreadable => "unreadable_body",
            Self::NotJson => "not_json",
            Self::NoModel => "no_model",
            Self::MaxWait => "bad_max_wait",
            Self::TooSlow => "body_timeout",
            Self::Priority => "bad_priority",
        })
    }
}

impl core::error::Error for BadRequest {}

impl BadRequest {
    pub(super) fn reply(self) -> Response<Body> {
        let status = match self {
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TooSlow => StatusCode::REQUEST_TIMEOUT,
            Self::Priority => {
                return reply::json(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": self.to_string(), "allowed": PRIORITIES }),
                );
            }
            _ => StatusCode::BAD_REQUEST,
        };
        reply::error(status, &self.to_string())
    }
}

/// How a request names its model
enum Route<'c> {
    /// By `model` in a JSON body, on a route of this API.
    Api(Api),
    /// By a path prefix, with the path the backend sees, which starts with `/`.
    Prefix { model: &'c Model, path: String },
}

fn route<'c>(config: &'c Config, method: &Method, uri: &Uri) -> Option<Route<'c>> {
    match (method, uri.path()) {
        (&Method::POST, "/v1/chat/completions" | "/v1/completions" | "/v1/embeddings") => {
            Some(Route::Api(Api::OpenAi))
        }
        (&Method::POST, "/v1/messages") => Some(Route::Api(Api::Anthropic)),
        (&Method::POST, "/api/chat" | "/api/generate" | "/api/embed" | "/api/embeddings") => {
            Some(Route::Api(Api::Ollama))
        }
        (_, path) => {
            let model = config.model_for_prefix(path)?;
            let rest = path.strip_prefix(model.prefix.as_deref()?)?;
            // Only a whole segment is stripped, so what is left stays a path on the backend.
            let rest = match rest {
                "" => "/",
                rest if rest.starts_with('/') => rest,
                _ => return None,
            };
            Some(Route::Prefix {
                model,
                path: rest.to_owned(),
            })
        }
    }
}

/// Routes `request` to its model, waits for the engine to admit it, and streams the answer back
///
/// Anything that is not a model's route is `404`.
pub(crate) async fn proxy(
    shared: &Shared,
    client: &Client,
    request: Request<Incoming>,
) -> Response<Body> {
    let config = Arc::clone(&shared.config.borrow());
    let Some(route) = route(&config, request.method(), request.uri()) else {
        return reply::error(StatusCode::NOT_FOUND, "not_found");
    };
    let (priority, max_wait) = match wait_of(request.headers(), config.max_wait) {
        Ok(wait) => wait,
        Err(bad) => return bad.reply(),
    };
    let (parts, body) = request.into_parts();
    let body = match read_body(body, shared.timeouts.body_read).await {
        Ok(body) => body,
        Err(bad) => return bad.reply(),
    };
    let (model, path, parsed) = match route {
        Route::Api(api) => {
            let (name, parsed) = match model_of(&body) {
                Ok(found) => found,
                Err(bad) => return bad.reply(),
            };
            let Some(model) = config.models.get(&name) else {
                return unknown(&config, &name);
            };
            if !model.apis.contains(&api) {
                return wrong_api(model);
            }
            (model, parts.uri.path().to_owned(), Some(parsed))
        }
        Route::Prefix { model, path } => (model, path, None),
    };
    let engine = &shared.engine;
    let admitted = engine
        .admit(client.name.clone(), model.name.clone(), priority, max_wait)
        .await;
    match admitted {
        Admission::Forward(in_flight) => {
            let body = for_backend(&model.backend, body, parsed);
            forward(&shared.http, model, parts, &path, body, in_flight).await
        }
        Admission::Refused(refusal) => reply::busy(&model.name, &refusal, &engine.clock()),
        Admission::Failed(error) => reply::json(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "failed", "model": model.name.as_str(), "reason": error }),
        ),
        Admission::Unknown => unknown(&config, &model.name),
    }
}

fn wait_of(headers: &HeaderMap, default: Duration) -> Result<(Priority, Duration), BadRequest> {
    let priority = match headers.get(PRIORITY) {
        None => Priority::Interactive,
        Some(value) if value == "interactive" => Priority::Interactive,
        Some(value) if value == "batch" => Priority::Batch,
        Some(_) => return Err(BadRequest::Priority),
    };
    let max_wait = match headers.get(MAX_WAIT) {
        None => default,
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|text| text.parse::<UpDuration>().ok())
            .map(UpDuration::as_duration)
            .ok_or(BadRequest::MaxWait)?,
    };
    Ok((priority, max_wait))
}

/// Reads `body` whole within `within`, refusing it once it passes [`MAX_BODY`]
///
/// # Errors
/// [`BadRequest::TooLarge`] past the cap, [`BadRequest::TooSlow`] when
/// `within` runs out, [`BadRequest::Unreadable`] when the body fails before
/// its end.
pub(super) async fn read_body<B>(body: B, within: Duration) -> Result<Bytes, BadRequest>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn core::error::Error + Send + Sync>>,
{
    // A declared length past the cap is answered before any of the body arrives.
    if body.size_hint().lower() > MAX_BODY as u64 {
        return Err(BadRequest::TooLarge);
    }
    match timeout(within, Limited::new(body, MAX_BODY).collect()).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(err)) if err.is::<LengthLimitError>() => Err(BadRequest::TooLarge),
        Ok(Err(_)) => Err(BadRequest::Unreadable),
        Err(_) => Err(BadRequest::TooSlow),
    }
}

/// The model a JSON body names, and the body parsed
///
/// # Errors
/// [`BadRequest::NotJson`] or [`BadRequest::NoModel`].
fn model_of(body: &[u8]) -> Result<(ModelName, Value), BadRequest> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| BadRequest::NotJson)?;
    let name = parsed
        .get("model")
        .and_then(Value::as_str)
        .ok_or(BadRequest::NoModel)?;
    Ok((ModelName::from(name), parsed))
}

pub(super) fn unknown(config: &Config, name: &ModelName) -> Response<Body> {
    let models: Vec<&str> = config.models.keys().map(ModelName::as_str).collect();
    reply::json(
        StatusCode::NOT_FOUND,
        json!({ "error": "unknown_model", "model": name.as_str(), "models": models }),
    )
}

fn wrong_api(model: &Model) -> Response<Body> {
    let apis: Vec<&str> = model
        .apis
        .iter()
        .map(|api| match api {
            Api::OpenAi => "openai",
            Api::Anthropic => "anthropic",
            Api::Ollama => "ollama",
        })
        .collect();
    reply::json(
        StatusCode::BAD_REQUEST,
        json!({ "error": "wrong_api", "model": model.name.as_str(), "apis": apis }),
    )
}

/// The body as the backend should see it
///
/// For ollama, a top-level `model` becomes ollama's own name, and a top-level
/// `keep_alive` (ADR 0001) and `options.num_ctx` go. The original bytes go
/// whenever nothing changed.
fn for_backend(backend: &Backend, original: Bytes, parsed: Option<Value>) -> Bytes {
    let Backend::Ollama { name, .. } = backend else {
        return original;
    };
    let mut parsed = match parsed {
        Some(parsed) => parsed,
        None => match serde_json::from_slice::<Value>(&original) {
            Ok(parsed) => parsed,
            Err(_) => return original,
        },
    };
    let Some(object) = parsed.as_object_mut() else {
        return original;
    };
    let mut changed = object.remove("keep_alive").is_some();
    // The configured name fixes the context, and the footprint was measured at
    // it. Another `num_ctx` makes ollama reload the model at another size.
    if let Some(options) = object.get_mut("options").and_then(Value::as_object_mut) {
        changed |= options.remove("num_ctx").is_some();
    }
    if let Some(model) = object.get_mut("model")
        && model.as_str() != Some(name.as_str())
    {
        *model = Value::from(name.as_str());
        changed = true;
    }
    if !changed {
        return original;
    }
    serde_json::to_vec(&parsed).map_or(original, Bytes::from)
}

fn hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str()) || name.as_str().starts_with("proxy-")
}

/// The client's headers the backend should see
///
/// `Host` and `Content-Length` are the backend request's own, and the
/// client's keys and the dog's own headers stay here.
fn to_backend(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let ours = [AUTHORIZATION, HOST, CONTENT_LENGTH].contains(name)
            || name == API_KEY
            || name == PRIORITY
            || name == MAX_WAIT;
        if !ours && !hop_by_hop(name) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// Where a request for `path` and `query` goes on the backend at `base`
///
/// Built field by field, so nothing a client sends can move it off the
/// base's scheme, host and port. `None` when `base` does not parse or the
/// result would leave it anyway.
fn target(base: &str, path: &str, query: Option<&str>) -> Option<Url> {
    let base = Url::parse(base).ok()?;
    let mut target = base.clone();
    target.set_path(&format!("{}{path}", base.path().trim_end_matches('/')));
    target.set_query(query);
    target.set_fragment(None);
    let same = target.host().is_some()
        && target.scheme() == base.scheme()
        && target.host() == base.host()
        && target.port_or_known_default() == base.port_or_known_default();
    same.then_some(target)
}

async fn forward(
    http: &reqwest::Client,
    model: &Model,
    parts: Parts,
    path: &str,
    body: Bytes,
    in_flight: InFlight,
) -> Response<Body> {
    let base = match &model.backend {
        Backend::Ollama { url, .. } => Some(url.as_str()),
        Backend::Sheep { .. } => model.url.as_deref(),
    };
    let unreachable = || {
        reply::json(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "unreachable", "model": model.name.as_str() }),
        )
    };
    let Some(base) = base else {
        return unreachable();
    };
    let mut headers = to_backend(&parts.headers);
    if let Some(key) = model.key() {
        let Ok(mut value) = HeaderValue::try_from(format!("Bearer {key}")) else {
            return reply::error(StatusCode::BAD_GATEWAY, "bad_model_key");
        };
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    let Some(target) = target(base, path, parts.uri.query()) else {
        return reply::json(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "bad_target", "model": model.name.as_str() }),
        );
    };
    let sent = http
        .request(parts.method, target)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let Ok(answer) = sent else {
        return unreachable();
    };
    let mut response = Response::new(Body::default());
    *response.status_mut() = answer.status();
    for (name, value) in answer.headers() {
        if !hop_by_hop(name) {
            response.headers_mut().append(name.clone(), value.clone());
        }
    }
    let frames = answer.bytes_stream().map(|chunk| {
        chunk
            .map(Frame::data)
            .map_err(|err| io::Error::other(err.without_url()))
    });
    *response.body_mut() = Streamed {
        inner: BodyExt::boxed(StreamBody::new(frames)),
        _in_flight: in_flight,
    }
    .boxed();
    response
}

/// A backend's response body, which ends its request's in-flight count when dropped
///
/// hyper drops it once the last frame is sent or the client hangs up, so
/// either one lets the model be unloaded.
#[derive(Debug)]
struct Streamed {
    inner: Body,
    _in_flight: InFlight,
}

impl hyper::body::Body for Streamed {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
