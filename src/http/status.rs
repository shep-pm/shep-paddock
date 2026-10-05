//! `GET /paddock/status` and `GET /v1/models`: the book as JSON, sizes in bytes and times in RFC 3339.

use hyper::{Response, StatusCode};
use serde_json::{Value, json};

use super::{Body, Shared, lease::render_id, reply};
use crate::{
    book::{Hold, Moment, Priority, Snapshot, State, WaiterKind},
    config::Config,
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

/// The status as JSON, with `host` for the totals and `clock` for the times
pub(super) fn status_body(snapshot: &Snapshot, host: &Host, clock: &Clock) -> Value {
    let time = |moment: Moment| clock.wall(moment).to_string();
    let declared_vram = match snapshot.declared.vram {
        Vram::None => 0,
        Vram::Bytes(bytes) => bytes,
        Vram::All => host.vram,
    };
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
            })
        })
        .collect();
    let leases: Vec<_> = snapshot
        .leases
        .iter()
        .map(|lease| {
            json!({
                "id": render_id(lease.id),
                "client": lease.client.as_str(),
                "model": lease.model.as_str(),
                "since": time(lease.since),
                "expected_until": lease.expected_until.map(time),
                "note": lease.note,
                "hold": match lease.hold {
                    Hold::Connection => "connection",
                    Hold::Heartbeat { .. } => "heartbeat",
                },
                "attached": lease.attached,
            })
        })
        .collect();
    let waiters: Vec<_> = snapshot
        .waiters
        .iter()
        .map(|waiter| {
            json!({
                "client": waiter.client.as_str(),
                "model": waiter.model.as_str(),
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
    json!({
        "host": {
            "vram_bytes": host.vram,
            "ram_bytes": host.ram,
            "vram_declared_bytes": declared_vram,
            "ram_declared_bytes": snapshot.declared.ram,
        },
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
                "loaded": state == State::Loaded,
                "state": state_text(state),
            })
        })
        .collect();
    json!({ "object": "list", "data": data })
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
