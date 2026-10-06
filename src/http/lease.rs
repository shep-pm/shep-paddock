//! The lease routes: take a lease, attach to it again, renew it or note its progress, release it.

use core::fmt;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    header::{ALLOW, CONTENT_TYPE, HeaderValue},
};
use serde::Deserialize;
use serde_json::json;
use shep_client::shep_core::values::UpDuration;

use super::{
    Body, Shared,
    proxy::{read_body, unknown},
    reply,
};
use crate::{
    book::{Hold, LeaseId, Priority},
    config::{Client, ModelName},
    engine::{LeaseEvent, LeaseEvents, LeaseRefused, LeaseRequest},
};
use stream::LeaseStream;

mod stream;
#[cfg(test)]
mod tests;

const PREFIX: &str = "/paddock/leases";
// The spec's default for a heartbeat lease.
const DEFAULT_TTL: Duration = Duration::from_secs(60);
// A heartbeat holder that vanishes keeps its model held for at most one ttl.
const MAX_TTL: Duration = Duration::from_secs(60 * 60);
// A note is a label for status and `state.json`, so a long one is a mistake.
const MAX_NOTE: usize = 1024;

/// Whether `path` is the lease collection or a whole segment under it
pub(super) fn is_route(path: &str) -> bool {
    path.strip_prefix(PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The id a client sees for `lease`
pub(crate) fn render_id(lease: LeaseId) -> String {
    format!("L{}", lease.0)
}

/// The lease an id names, or `None` unless it is exactly what [`render_id`] makes
pub(crate) fn parse_id(text: &str) -> Option<LeaseId> {
    let digits = text.strip_prefix('L')?;
    let canonical = !digits.starts_with('0') || digits == "0";
    if digits.is_empty() || !canonical || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(LeaseId)
}

/// A duration as a client reads it back: whole seconds, or milliseconds when it has a fraction
fn duration_text(duration: Duration) -> String {
    if duration.subsec_millis() == 0 && duration.subsec_nanos() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PriorityText {
    Interactive,
    Batch,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HoldText {
    Connection,
    Heartbeat,
}

/// The body of `POST /paddock/leases`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Take {
    model: String,
    priority: Option<PriorityText>,
    expected: Option<String>,
    note: Option<String>,
    hold: Option<HoldText>,
    ttl: Option<String>,
    max_wait: Option<String>,
    release_if_idle: Option<String>,
    reclaimable: Option<bool>,
}

/// The body of a `PUT`, which renews without a `note` and records one with it
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    note: Option<String>,
}

/// Why a take or a note was answered with a `400`
#[derive(Debug)]
enum BadTake {
    /// The body is not the shape above.
    Body(String),
    /// A duration is not in shep's `UpDuration` grammar.
    Duration(&'static str),
    /// A heartbeat lease's `ttl` is longer than [`MAX_TTL`].
    TtlTooLong,
    /// `note` is longer than [`MAX_NOTE`] bytes.
    NoteTooLong,
    /// `release_if_idle` is 0, which would end the lease at its grant.
    IdleZero,
}

impl BadTake {
    /// The `error` the `400` names
    fn code(&self) -> &'static str {
        match self {
            Self::Body(_) | Self::Duration(_) | Self::IdleZero => "bad_lease_request",
            Self::TtlTooLong => "bad_ttl",
            Self::NoteTooLong => "note_too_long",
        }
    }
}

impl fmt::Display for BadTake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(why) => f.write_str(why),
            Self::Duration(field) => write!(f, "{field} is not a duration such as 30s or 8h"),
            Self::TtlTooLong => write!(f, "ttl is at most {}", duration_text(MAX_TTL)),
            Self::NoteTooLong => write!(f, "note is at most {MAX_NOTE} bytes"),
            Self::IdleZero => f.write_str("release_if_idle must be more than 0"),
        }
    }
}

impl core::error::Error for BadTake {}

fn duration(field: &'static str, text: Option<&str>) -> Result<Option<Duration>, BadTake> {
    text.map(|text| {
        text.parse::<UpDuration>()
            .map(UpDuration::as_duration)
            .map_err(|_| BadTake::Duration(field))
    })
    .transpose()
}

impl Take {
    fn parse(body: &[u8]) -> Result<Self, BadTake> {
        serde_json::from_slice(body).map_err(|err| BadTake::Body(err.to_string()))
    }

    /// What to ask the engine for, and the `ttl` a heartbeat lease will be told
    fn request(self) -> Result<(LeaseRequest, Duration), BadTake> {
        let ttl = duration("ttl", self.ttl.as_deref())?.unwrap_or(DEFAULT_TTL);
        if self.note.as_ref().is_some_and(|note| note.len() > MAX_NOTE) {
            return Err(BadTake::NoteTooLong);
        }
        let hold = match self.hold {
            None | Some(HoldText::Connection) => Hold::Connection,
            Some(HoldText::Heartbeat) if ttl > MAX_TTL => return Err(BadTake::TtlTooLong),
            Some(HoldText::Heartbeat) => Hold::Heartbeat { ttl },
        };
        let release_if_idle = duration("release_if_idle", self.release_if_idle.as_deref())?;
        if release_if_idle == Some(Duration::ZERO) {
            return Err(BadTake::IdleZero);
        }
        let priority = match self.priority {
            Some(PriorityText::Interactive) => Priority::Interactive,
            None | Some(PriorityText::Batch) => Priority::Batch,
        };
        let request = LeaseRequest {
            model: ModelName::from(self.model),
            priority,
            expected: duration("expected", self.expected.as_deref())?,
            max_wait: duration("max_wait", self.max_wait.as_deref())?,
            hold,
            note: self.note,
            reclaimable: self.reclaimable.unwrap_or(false),
            release_if_idle,
        };
        Ok((request, ttl))
    }
}

fn bad_take(bad: &BadTake) -> Response<Body> {
    reply::json(
        StatusCode::BAD_REQUEST,
        json!({ "error": bad.code(), "detail": bad.to_string() }),
    )
}

/// The reply for a refused lease call
///
/// Another client's lease answers as an unknown id does, so these routes
/// do not tell the two apart.
fn refused(why: LeaseRefused) -> Response<Body> {
    match why {
        LeaseRefused::NotFound | LeaseRefused::NotYours => {
            reply::error(StatusCode::NOT_FOUND, "not_found")
        }
        LeaseRefused::Attached => reply::error(StatusCode::CONFLICT, "attached"),
    }
}

fn no_content() -> Response<Body> {
    let mut response = Response::new(
        http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
}

fn streamed(stream: LeaseStream) -> Response<Body> {
    let mut response = Response::new(stream.boxed());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    response
}

/// Answers a request under `/paddock/leases`
pub(super) async fn handle(
    shared: &Shared,
    client: &Client,
    request: Request<Incoming>,
) -> Response<Body> {
    let path = request.uri().path().to_owned();
    // `is_route` already routed only here, so this defends a caller that skips it.
    let segments: Vec<&str> = match path.strip_prefix(PREFIX) {
        Some("") => Vec::new(),
        Some(rest) if rest.starts_with('/') => rest[1..].split('/').collect(),
        _ => return reply::error(StatusCode::NOT_FOUND, "not_found"),
    };
    // An empty segment names no lease path, so no method is allowed on it.
    if segments.contains(&"") {
        return reply::error(StatusCode::NOT_FOUND, "not_found");
    }
    match (request.method(), segments.as_slice()) {
        (&Method::POST, []) => take(shared, client, request).await,
        (&Method::POST, [id, "attach"]) => match parse_id(id) {
            Some(lease) => attach(shared, client, lease).await,
            None => refused(LeaseRefused::NotFound),
        },
        (&Method::PUT, [id]) => match parse_id(id) {
            Some(lease) => renew_or_note(shared, client, lease, request).await,
            None => refused(LeaseRefused::NotFound),
        },
        (&Method::DELETE, [id]) => match parse_id(id) {
            Some(lease) => answer(shared.engine.release(client.name.clone(), lease).await),
            None => refused(LeaseRefused::NotFound),
        },
        (_, [] | [_, "attach"]) => not_allowed("POST"),
        (_, [_]) => not_allowed("PUT, DELETE"),
        _ => reply::error(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// The `405` for a lease path asked with a method it does not take
fn not_allowed(allow: &'static str) -> Response<Body> {
    let mut response = reply::error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static(allow));
    response
}

fn answer(result: Result<(), LeaseRefused>) -> Response<Body> {
    result.map_or_else(refused, |()| no_content())
}

/// Renews a lease, or records a note on it when the body carries one
///
/// An empty body and `{}` both renew, so a client that always sends JSON can renew.
async fn renew_or_note(
    shared: &Shared,
    client: &Client,
    lease: LeaseId,
    request: Request<Incoming>,
) -> Response<Body> {
    let body = match read_body(request.into_body(), shared.timeouts.body_read).await {
        Ok(body) => body,
        Err(bad) => return bad.reply(),
    };
    let note = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<Note>(&body) {
            Ok(note) => note.note,
            Err(err) => return bad_take(&BadTake::Body(err.to_string())),
        }
    };
    let Some(note) = note else {
        return answer(shared.engine.renew(client.name.clone(), lease).await);
    };
    if note.len() > MAX_NOTE {
        return bad_take(&BadTake::NoteTooLong);
    }
    answer(shared.engine.note(client.name.clone(), lease, note).await)
}

async fn attach(shared: &Shared, client: &Client, lease: LeaseId) -> Response<Body> {
    match shared.engine.attach(client.name.clone(), lease).await {
        Ok(events) => streamed(LeaseStream::new(
            events,
            None,
            shared.config.clone(),
            shared.engine.clock(),
        )),
        Err(why) => refused(why),
    }
}

async fn take(shared: &Shared, client: &Client, request: Request<Incoming>) -> Response<Body> {
    let body = match read_body(request.into_body(), shared.timeouts.body_read).await {
        Ok(body) => body,
        Err(bad) => return bad.reply(),
    };
    let take = match Take::parse(&body) {
        Ok(take) => take,
        Err(bad) => return bad_take(&bad),
    };
    let (ask, ttl) = match take.request() {
        Ok(ask) => ask,
        Err(bad) => return bad_take(&bad),
    };
    let config = shared.config.borrow().clone();
    if !config.models.contains_key(&ask.model) {
        return unknown(&config, &ask.model);
    }
    let model = ask.model.clone();
    let heartbeat = matches!(ask.hold, Hold::Heartbeat { .. });
    let events = shared.engine.take_lease(client.name.clone(), ask).await;
    if !heartbeat {
        let stream = LeaseStream::new(
            events,
            Some(model),
            shared.config.clone(),
            shared.engine.clock(),
        );
        return streamed(stream);
    }
    granted_or_turned_away(shared, model, ttl, events).await
}

/// Waits for a heartbeat lease's grant, which a hang-up cancels by dropping this future
async fn granted_or_turned_away(
    shared: &Shared,
    model: ModelName,
    ttl: Duration,
    mut events: LeaseEvents,
) -> Response<Body> {
    while let Some(event) = events.recv().await {
        match event {
            LeaseEvent::Waiting { .. } => {}
            LeaseEvent::Granted { lease } => {
                return reply::json(
                    StatusCode::OK,
                    json!({ "id": render_id(lease), "ttl": duration_text(ttl) }),
                );
            }
            LeaseEvent::Refused(refusal) => {
                return reply::busy(&model, &refusal, &shared.engine.clock());
            }
            LeaseEvent::Failed(error) => {
                return reply::json(
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": "failed", "model": model.as_str(), "reason": error }),
                );
            }
            LeaseEvent::Ended(_) => break,
        }
    }
    reply::json(
        StatusCode::BAD_GATEWAY,
        json!({ "error": "failed", "model": model.as_str(), "reason": "the lease ended before it was granted" }),
    )
}
