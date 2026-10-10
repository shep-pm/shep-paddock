//! `POST /paddock/leases/{id}/revoke`: an admin client ends any client's lease.

use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;

use super::{BadTake, MAX_NOTE, answer, bad_take, refused};
use crate::{
    book::LeaseId,
    config::Client,
    engine::LeaseRefused,
    http::{Body, Shared, proxy::read_body, reply},
};

/// The body of a revoke, which may also be empty
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    reason: Option<String>,
}

/// Revokes `lease`, `None` for an id that names no lease, as `client`
///
/// A client that is not an admin is refused before anything else is read.
pub(super) async fn revoke(
    shared: &Shared,
    client: &Client,
    lease: Option<LeaseId>,
    request: Request<Incoming>,
) -> Response<Body> {
    if !client.admin {
        return reply::error(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(lease) = lease else {
        return refused(LeaseRefused::NotFound);
    };
    let body = match read_body(request.into_body(), shared.timeouts.body_read).await {
        Ok(body) => body,
        Err(bad) => return bad.reply(),
    };
    let reason = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<Revoke>(&body) {
            Ok(revoke) => revoke.reason,
            Err(err) => return bad_take(&BadTake::Body(err.to_string())),
        }
    };
    if reason
        .as_ref()
        .is_some_and(|reason| reason.len() > MAX_NOTE)
    {
        return bad_take(&BadTake::ReasonTooLong);
    }
    answer(
        shared
            .engine
            .revoke(client.name.clone(), lease, reason)
            .await,
    )
}
