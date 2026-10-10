//! The JSON replies the endpoint gives for itself.

use std::time::Duration;

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
    let mut response = json(
        StatusCode::SERVICE_UNAVAILABLE,
        busy_body(Some(model), refusal, clock),
    );
    if let Some(after) = refusal.retry_after {
        let seconds = after.as_secs() + u64::from(after.subsec_nanos() > 0);
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

/// The JSON a refusal is told as, in a `503` or on a lease's stream
///
/// `model` is `None` where the caller does not know it, and the body then says `null`.
pub(crate) fn busy_body(
    model: Option<&ModelName>,
    refusal: &Refusal,
    clock: &Clock,
) -> serde_json::Value {
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
    json!({
        "error": "busy",
        "model": model.map(ModelName::as_str),
        "reason": sentence(&refusal.reason, clock),
        "reason_kind": kind(&refusal.reason),
        "expected_until": expected.map(|at| at.to_string()),
    })
}

/// `duration` in its largest whole unit: hours from an hour, minutes from a minute, else seconds
pub(crate) fn rough(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        3_600.. => format!("{}h", seconds / 3_600),
        60.. => format!("{}m", seconds / 60),
        _ => format!("{seconds}s"),
    }
}

/// Why a waiter waits, as a sentence
///
/// A held model's idle time is read off `clock` as the sentence is written, and is left out
/// while its lease is in use.
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
            idle_since,
            ..
        } => {
            let idle = idle_since
                .map(|idle_since| format!(", idle for {}", rough(clock.moment().since(idle_since))))
                .unwrap_or_default();
            format!(
                "{model} is held by {} since {}{idle}",
                client.as_str(),
                clock.wall(*since)
            )
        }
        Reason::Behind { model } => format!("{model} is loading or claimed by another waiter"),
        Reason::Turn {
            model,
            holders,
            ahead,
        } => {
            let holders: Vec<_> = holders
                .iter()
                .map(|holder| match &holder.note {
                    Some(note) => format!("{} {note:?}", holder.client.as_str()),
                    None => holder.client.as_str().to_owned(),
                })
                .collect();
            let place = match ahead {
                0 => "next in line".to_owned(),
                ahead => format!("{ahead} ahead"),
            };
            format!("{model} is serving {}, {place}", holders.join(" and "))
        }
    }
}

/// What a waiter waits on, as one word a client can match on
// wire format: clients match on these words, so changing one is a breaking change.
pub(crate) fn kind(reason: &Reason) -> &'static str {
    match reason {
        Reason::Loading { .. } => "loading",
        Reason::Evicting { .. } => "evicting",
        Reason::Draining { .. } => "draining",
        Reason::Grace { .. } => "grace",
        Reason::Held { .. } => "held",
        Reason::Behind { .. } => "behind",
        Reason::Turn { .. } => "turn",
    }
}

#[cfg(test)]
mod tests;
