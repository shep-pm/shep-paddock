//! The JSON replies the endpoint gives for itself.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Response, StatusCode,
    header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER},
};
use serde_json::json;

use super::Body;
use crate::{
    book::{Reason, Refusal},
    config::ModelName,
    engine::Clock,
};

/// A response with a JSON body
pub(crate) fn json(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let body = Full::new(Bytes::from(bytes))
        .map_err(|never| match never {})
        .boxed();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// A response whose body is `{"error": message}`
pub(crate) fn error(status: StatusCode, message: &str) -> Response<Body> {
    json(status, json!({ "error": message }))
}

/// The `503` for a request or lease that was turned away
///
/// `Retry-After` is whole seconds, rounded up, and present only when the
/// refusal says when to try again. Times are RFC 3339 in UTC.
pub(crate) fn busy(model: &ModelName, refusal: &Refusal, clock: &Clock) -> Response<Body> {
    let now = clock.wall(clock.moment());
    let expected = match (&refusal.reason, refusal.retry_after) {
        (
            Reason::Held {
                until: Some(until), ..
            },
            _,
        ) => Some(clock.wall(*until)),
        (_, Some(after)) => now.checked_add(after).ok(),
        (_, None) => None,
    };
    let mut response = json(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({
            "error": "busy",
            "model": model.as_str(),
            "reason": sentence(&refusal.reason, clock),
            "expected_until": expected.map(|at| at.to_string()),
        }),
    );
    if let Some(after) = refusal.retry_after {
        let seconds = after.as_secs() + u64::from(after.subsec_nanos() > 0);
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

/// Why a waiter waits, as a sentence
pub(crate) fn sentence(reason: &Reason, clock: &Clock) -> String {
    match reason {
        Reason::Loading { model } => format!("{model} is loading"),
        Reason::Evicting { model, for_model } => format!("{model} is evicting for {for_model}"),
        Reason::Draining { model } => format!("{model} is unloading"),
        Reason::Grace { model, until } => {
            format!(
                "{model} is in its grace period until {}",
                clock.wall(*until)
            )
        }
        Reason::Held {
            model,
            client,
            since,
            ..
        } => format!(
            "{model} is held by {} since {}",
            client.as_str(),
            clock.wall(*since)
        ),
        Reason::Behind { model } => format!("{model} is loading or claimed by another waiter"),
    }
}
