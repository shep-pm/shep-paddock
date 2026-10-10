//! The lease routes: take a lease, attach to it again, renew it or note its progress, release it,
//! revoke it.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    header::{ALLOW, CONTENT_TYPE, HeaderValue},
};
use serde_json::json;

use super::{
    Body, Peer, Shared,
    proxy::{read_body, unknown},
    reply,
};
use crate::{
    book::{Hold, LeaseId, Leased},
    config::{Client, ModelName},
    engine::{LeaseEvent, LeaseEvents, LeaseRefused},
};
use stream::LeaseStream;
use take::{BadTake, MAX_NOTE, Note, Take, bad_take};

mod revoke;
pub(crate) mod stream;
mod take;
#[cfg(test)]
mod tests;

const PREFIX: &str = "/paddock/leases";
/// Whether `path` is the lease collection or a whole segment under it
pub(super) fn is_route(path: &str) -> bool {
    path.strip_prefix(PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The id a client sees for `lease`
pub(crate) fn render_id(lease: LeaseId) -> String {
    lease.to_string()
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
        LeaseRefused::Protected => reply::error(StatusCode::FORBIDDEN, "protected"),
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
        (&Method::POST, [id, "revoke"]) => {
            revoke::revoke(shared, client, parse_id(id), request).await
        }
        (_, [] | [_, "attach" | "revoke"]) => not_allowed("POST"),
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
    let loopback = request
        .extensions()
        .get::<Peer>()
        .is_some_and(|peer| peer.is_loopback());
    let body = match read_body(request.into_body(), shared.timeouts.body_read).await {
        Ok(body) => body,
        Err(bad) => return bad.reply(),
    };
    let take = match Take::parse(&body) {
        Ok(take) => take,
        Err(bad) => return bad_take(&bad),
    };
    let (ask, ttl) = match take.request(loopback) {
        Ok(ask) => ask,
        Err(bad) => return bad_take(&bad),
    };
    let config = shared.config.borrow().clone();
    let model = match &ask.leased {
        Leased::Model(model) if !config.models.contains_key(model) => {
            return unknown(&config, model);
        }
        Leased::Model(model) => Some(model.clone()),
        Leased::Bare { footprint, .. } if !config.host.ever_fits(footprint) => {
            return bad_take(&BadTake::NeverFits);
        }
        Leased::Bare { .. } => None,
    };
    let heartbeat = matches!(ask.hold, Hold::Heartbeat { .. });
    let events = shared.engine.take_lease(client.name.clone(), ask).await;
    if !heartbeat {
        let stream = LeaseStream::new(events, model, shared.config.clone(), shared.engine.clock());
        return streamed(stream);
    }
    granted_or_turned_away(shared, model, ttl, events).await
}

/// Waits for a heartbeat lease's grant, which a hang-up cancels by dropping this future
async fn granted_or_turned_away(
    shared: &Shared,
    model: Option<ModelName>,
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
                return reply::busy(model.as_ref(), &refusal, &shared.engine.clock());
            }
            LeaseEvent::Failed(error) => {
                return reply::json(
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": "failed", "model": model.as_ref().map(ModelName::as_str), "reason": error }),
                );
            }
            LeaseEvent::Ended(_) | LeaseEvent::Revoked(_) => break,
        }
    }
    reply::json(
        StatusCode::BAD_GATEWAY,
        json!({ "error": "failed", "model": model.as_ref().map(ModelName::as_str), "reason": "the lease ended before it was granted" }),
    )
}
