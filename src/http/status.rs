//! `GET /paddock/status`, `GET /v1/models` and `GET /api/tags`: the book as JSON, sizes in bytes and
//! times in RFC 3339.

use std::time::Duration;

use hyper::{Response, StatusCode};
use serde_json::{Value, json};

use super::{Body, Shared, lease::render_id, reply};
use crate::{
    book::{Hold, Moment, Priority, Snapshot, State, WaiterKind},
    config::{Api, Config, ModelName, PlacementName},
    engine::Clock,
    footprint::{Host, Vram},
};

/// The status endpoint's answer
pub(super) async fn status(state: &Shared, config: &Config) -> Response<Body> {
    let snapshot = state.engine.snapshot().await;
    let clock = state.engine.clock();
    reply::json(StatusCode::OK, status_body(&snapshot, &config.host, &clock))
}

/// The model list's answer
pub(super) async fn models(state: &Shared, config: &Config) -> Response<Body> {
    let snapshot = state.engine.snapshot().await;
    reply::json(StatusCode::OK, models_body(&snapshot, config))
}

/// ollama's model list, so a client of ollama's own API finds its models here
pub(super) fn tags(config: &Config) -> Response<Body> {
    reply::json(StatusCode::OK, tags_body(config))
}

/// The status as JSON, with `host` for the totals and `clock` for the times
pub(super) fn status_body(snapshot: &Snapshot, host: &Host, clock: &Clock) -> Value {
    let now = clock.moment();
    let time = |moment: Moment| clock.wall(moment).to_string();
    let declared_vram = vram_bytes(snapshot.declared.vram, host);
    let models: Vec<_> = snapshot
        .models
        .iter()
        .map(|model| {
            json!({
                "model": model.name.as_str(),
                "state": state_text(model.state),
                "in_flight": model.in_flight,
                // Moment(0) is a model never used since the dog started.
                "last_used": (model.last_used != Moment(0)).then(|| time(model.last_used)),
                "held_by": model.held_by.iter().map(|client| client.as_str()).collect::<Vec<_>>(),
                "unknown": model.unknown,
                "placement": model.placement.as_ref().map(PlacementName::as_str),
                "stray": model.stray,
                "measured": {
                    "vram_bytes": model.measured.vram,
                    "ram_bytes": model.measured.ram,
                },
                "drift": model.drift,
            })
        })
        .collect();
    let leases: Vec<_> = snapshot
        .leases
        .iter()
        .map(|lease| {
            let idle_for = if lease.in_use {
                Duration::ZERO
            } else {
                now.since(lease.last_activity)
            };
            json!({
                "id": render_id(lease.id),
                "client": lease.client.as_str(),
                "model": lease.model.as_ref().map(ModelName::as_str),
                "since": time(lease.since),
                "expected_until": lease.expected_until.map(time),
                "note": lease.note,
                "hold": match lease.hold {
                    Hold::Connection => "connection",
                    Hold::Heartbeat { .. } => "heartbeat",
                },
                "attached": lease.attached,
                "last_activity": time(lease.last_activity),
                "idle_for": idle_for.as_secs(),
                "release_if_idle": lease.release_if_idle.map(whole_seconds_up),
                "reclaimable": lease.reclaimable,
                "footprint": lease.footprint.map(|footprint| json!({
                    "vram_bytes": vram_bytes(footprint.vram, host),
                    "ram_bytes": footprint.ram,
                })),
                "measured": lease.footprint.and(lease.measured.vram).map(|vram| json!({
                    "vram_bytes": vram,
                    "ram_bytes": null,
                })),
                "drift": lease.drift,
                "revoked": lease.revoked.as_ref().map(|revoked| json!({
                    "by": revoked.by.as_str(),
                    "note": revoked.note,
                })),
            })
        })
        .collect();
    let waiters: Vec<_> = snapshot
        .waiters
        .iter()
        .map(|waiter| {
            json!({
                "client": waiter.client.as_str(),
                "model": waiter.model.as_ref().map(ModelName::as_str),
                "kind": match waiter.kind {
                    WaiterKind::Request => "request",
                    WaiterKind::Lease => "lease",
                },
                "priority": match waiter.priority {
                    Priority::Interactive => "interactive",
                    Priority::Batch => "batch",
                },
                "since": time(waiter.since),
                "reason": waiter.reason.as_ref().map(|reason| reply::sentence(reason, clock)),
                "reason_kind": waiter.reason.as_ref().map(reply::kind),
                "estimate": waiter.estimate.map(time),
            })
        })
        .collect();
    let errors: Vec<_> = snapshot
        .errors
        .iter()
        .map(|error| {
            json!({
                "model": error.model.as_str(),
                "at": time(error.at),
                "error": error.error,
            })
        })
        .collect();
    let mut totals = json!({
        "vram_bytes": host.vram,
        "ram_bytes": host.ram,
        "vram_declared_bytes": declared_vram,
        "ram_declared_bytes": snapshot.declared.ram,
    });
    if let Some(unaccounted) = snapshot.unaccounted_vram {
        totals["unaccounted_vram_bytes"] = json!(unaccounted);
    }
    json!({
        "host": totals,
        "models": models,
        "leases": leases,
        "waiters": waiters,
        "errors": errors,
    })
}

/// Every configured model, whether it is loaded, and its state
///
/// Only models a client can ask for are listed, so an unknown sheep is not.
pub(super) fn models_body(snapshot: &Snapshot, config: &Config) -> Value {
    let data: Vec<_> = config
        .models
        .keys()
        .map(|name| {
            let state = snapshot
                .models
                .iter()
                .find(|model| model.name == *name)
                .map_or(State::Unloaded, |model| model.state);
            json!({
                "id": name.as_str(),
                "object": "model",
                // Typed OpenAI clients require both. The dog has no creation time to give.
                "created": 0,
                "owned_by": "paddock",
                "loaded": state == State::Loaded,
                "state": state_text(state),
            })
        })
        .collect();
    json!({ "object": "list", "data": data })
}

/// Every model that speaks ollama's API, by the name a client asks for
pub(super) fn tags_body(config: &Config) -> Value {
    let models: Vec<_> = config
        .models
        .values()
        .filter(|model| model.apis.contains(&Api::Ollama))
        .map(|model| json!({ "name": model.name.as_str(), "model": model.name.as_str() }))
        .collect();
    json!({ "models": models })
}

/// `vram` in bytes, where `all` is the host's whole VRAM and none is 0
fn vram_bytes(vram: Vram, host: &Host) -> u64 {
    match vram {
        Vram::None => 0,
        Vram::Bytes(bytes) => bytes,
        Vram::All => host.vram,
    }
}

/// `duration` in whole seconds, rounded up so a duration that is set never reads 0
fn whole_seconds_up(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
}

fn state_text(state: State) -> &'static str {
    match state {
        State::Unloaded => "unloaded",
        State::Reserved => "reserved",
        State::Loading => "loading",
        State::Loaded => "loaded",
        State::Evicting => "evicting",
        State::Unloading => "unloading",
    }
}

#[cfg(test)]
mod tests;
