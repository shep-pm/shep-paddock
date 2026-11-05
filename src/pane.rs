//! The pane lookout draws for the dog: the host's VRAM and RAM, each model, and each lease, with a
//! `revoke` on a lease's row.
//!
//! The shepherd's channel runs each action handler on a thread of its own and the engine answers
//! on the dog's runtime, so the pane reads the last snapshot [`Latest`] holds rather than asking
//! the engine. `metrics::report` refreshes it every [`REPORT_EVERY`](crate::metrics::REPORT_EVERY),
//! and a revoke refreshes it before it answers, since lookout pulls the pane again once the
//! answer is in.

use std::sync::Arc;

use serde::Deserialize;
use shep_channel::Shepherd;
use shep_client::dogs::{Destructive, PANE_ACTION, Pane, Row, pane_answer};
use tokio::{runtime::Handle, sync::watch};

use crate::{
    book::{LeaseView, ModelView, Moment, Snapshot},
    cli::status::size,
    config::{ClientName, Config},
    engine::{Clock, EngineHandle},
    footprint::{Host, Vram},
    http::{
        lease::{parse_id, render_id},
        status::{state_text, vram_bytes},
    },
};

/// The action a row of the leases table triggers, as `shep trigger` names it
pub(crate) const REVOKE: &str = "revoke";

/// The client a pane's revoke is recorded as
///
/// The engine lets a client revoke its own protected lease, comparing names, so this is a name no
/// client config would write: the pane must still be refused a protected client's lease.
pub(crate) const OPERATOR: &str = "<operator>";

/// How a cell shows a value there is none of
const EMPTY: &str = "-";

/// The last snapshot of the book, for the pane to read from any thread
#[derive(Debug, Clone)]
pub(crate) struct Latest(Arc<watch::Sender<Option<Snapshot>>>);

impl Latest {
    /// A holder with no snapshot yet
    pub(crate) fn new() -> Self {
        Self(Arc::new(watch::channel(None).0))
    }

    /// Replaces the snapshot the pane shows
    pub(crate) fn publish(&self, snapshot: Snapshot) {
        self.0.send_replace(Some(snapshot));
    }
}

/// The pane for `snapshot`, with `host` for the totals and `clock` for the times
pub(crate) fn build(snapshot: &Snapshot, host: &Host, clock: &Clock) -> Pane {
    let declared_vram = vram_bytes(snapshot.declared.vram, host);
    let mut totals = vec![
        (
            "VRAM".to_owned(),
            format!("{} of {} declared", size(declared_vram), size(host.vram)),
        ),
        (
            "RAM".to_owned(),
            format!(
                "{} of {} declared",
                size(snapshot.declared.ram),
                size(host.ram)
            ),
        ),
    ];
    if let Some(unaccounted) = snapshot.unaccounted_vram {
        totals.push(("VRAM unaccounted".to_owned(), size(unaccounted)));
    }
    Pane::new()
        .fields("host", totals)
        .table(
            "models",
            [
                "MODEL",
                "STATE",
                "VRAM",
                "RAM",
                "IN FLIGHT",
                "HELD BY",
                "LAST USED",
            ],
            snapshot
                .models
                .iter()
                .map(|model| model_row(model, host, clock)),
        )
        .table(
            "leases",
            ["LEASE", "CLIENT", "MODEL", "STATE", "SINCE"],
            snapshot.leases.iter().map(|lease| lease_row(lease, clock)),
        )
        .row_action(REVOKE, "End a lease", Destructive::Yes)
}

fn model_row(model: &ModelView, host: &Host, clock: &Clock) -> Row {
    let vram = match model.footprint.vram {
        Vram::All => "all".to_owned(),
        vram => size(vram_bytes(vram, host)),
    };
    let held_by = if model.held_by.is_empty() {
        EMPTY.to_owned()
    } else {
        model
            .held_by
            .iter()
            .map(ClientName::as_str)
            .collect::<Vec<_>>()
            .join(",")
    };
    let last_used = if model.last_used == Moment(0) {
        EMPTY.to_owned()
    } else {
        clock.wall(model.last_used).to_string()
    };
    Row::new(
        model.name.as_str(),
        [
            model.name.as_str().to_owned(),
            state_text(model.state).to_owned(),
            vram,
            size(model.footprint.ram),
            model.in_flight.to_string(),
            held_by,
            last_used,
        ],
    )
}

fn lease_row(lease: &LeaseView, clock: &Clock) -> Row {
    let id = render_id(lease.id);
    let state = if lease.revoked.is_some() {
        "revoked"
    } else {
        "granted"
    };
    Row::new(
        id.clone(),
        [
            id,
            lease.client.as_str().to_owned(),
            lease
                .model
                .as_ref()
                .map_or_else(|| EMPTY.to_owned(), |model| model.as_str().to_owned()),
            state.to_owned(),
            clock.wall(lease.since).to_string(),
        ],
    )
}

/// The answer to [`PANE_ACTION`]: the pane for the last snapshot in `latest`
///
/// Before the first snapshot it says the dog is starting.
pub(crate) fn answer(latest: &Latest, host: &Host, clock: &Clock, params: Option<&str>) -> String {
    pane_answer(params, || match &*latest.0.borrow() {
        Some(snapshot) => build(snapshot, host, clock),
        None => Pane::new().fields("host", [("STATUS", "starting")]),
    })
}

/// The params of a row action
#[derive(Debug, Deserialize)]
struct RowParams {
    row: String,
}

/// Revokes the lease `params` names as [`OPERATOR`], and says what happened
///
/// The same engine call as the HTTP route, so a protected client's lease is refused. The pane
/// takes no admin client: whoever triggers the action already holds the shepherd. The snapshot
/// `latest` holds is refreshed once the engine has answered.
pub(crate) async fn revoke(engine: &EngineHandle, latest: &Latest, params: Option<&str>) -> String {
    let lease = params
        .and_then(|params| serde_json::from_str::<RowParams>(params).ok())
        .and_then(|params| parse_id(&params.row));
    let Some(lease) = lease else {
        return crate::engine::LeaseRefused::NotFound.to_string();
    };
    match engine.revoke(ClientName::from(OPERATOR), lease, None).await {
        Ok(()) => {
            latest.publish(engine.snapshot().await);
            format!("revoked {}", render_id(lease))
        }
        Err(refused) => refused.to_string(),
    }
}

/// Answers [`PANE_ACTION`] and [`REVOKE`] on `shepherd`
///
/// `runtime` is the dog's, which a revoke waits on from the channel's handler thread.
pub(crate) fn register(
    shepherd: &Shepherd,
    engine: EngineHandle,
    config: watch::Receiver<Arc<Config>>,
    latest: Latest,
    runtime: Handle,
) {
    let pane_engine = engine.clone();
    let pane_latest = latest.clone();
    shepherd.on_action(PANE_ACTION, move |params, _| {
        let host = config.borrow().host;
        answer(&pane_latest, &host, &pane_engine.clock(), params)
    });
    shepherd.on_action(REVOKE, move |params, _| {
        runtime.block_on(revoke(&engine, &latest, params))
    });
}

#[cfg(test)]
mod tests;
