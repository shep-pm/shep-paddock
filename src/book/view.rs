//! A granted lease, as the status reports it.

use std::time::Duration;

use super::{Hold, LeaseId, Moment, Priority, Revocation};
use crate::{
    config::{ClientName, ModelName},
    footprint::Footprint,
    survey::Measured,
};

/// A granted lease, as the status reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseView {
    /// The lease.
    pub id: LeaseId,
    /// Who holds it.
    pub client: ClientName,
    /// The model it holds, or `None` for a bare lease.
    pub model: Option<ModelName>,
    /// What a bare lease declares, or `None` for a model lease.
    pub footprint: Option<Footprint>,
    /// The process a bare lease's job runs under, when the dog may read it.
    pub pid: Option<u32>,
    /// Where it queued, and where its model's reload queues after a crash, for a held lease.
    pub priority: Priority,
    /// When it was granted.
    pub since: Moment,
    /// When its holder expects to release it, if it said.
    pub expected_until: Option<Moment>,
    /// What the holder says it is for.
    pub note: Option<String>,
    /// How its holder shows it is still alive.
    pub hold: Hold,
    /// Whether a connection holder's stream is open. Heartbeat leases count as attached.
    pub attached: bool,
    /// Whether it keeps its model loaded without holding it.
    pub reclaimable: bool,
    /// The later of its grant, its holder's last request for its model, and its last note.
    pub last_activity: Moment,
    /// Whether a request of its holder's for its model is in flight or queued.
    pub in_use: bool,
    /// How long it may sit idle before it ends, if it asked.
    pub release_if_idle: Option<Duration>,
    /// Who revoked it and why, for a bare lease still listed after its revoke.
    pub revoked: Option<Revocation>,
    /// What the last survey measured a bare lease's job holding. The book leaves it unmeasured.
    pub measured: Measured,
    /// Whether the last survey measured a bare lease above its footprint. The book leaves it `false`.
    pub drift: bool,
}
