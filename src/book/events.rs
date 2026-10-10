//! What the book hears, and what it tells the engine to do.

use std::time::Duration;

use super::{Ended, LeaseAsk, LeaseId, Moment, Priority, Reason, Refusal, WaiterId};
use crate::{
    config::{Backend, ClientName, ModelName},
    footprint::Footprint,
};

/// Something that happened, for the Book to decide on
#[derive(Debug)]
pub(crate) enum Event {
    /// A request for `model` arrived.
    RequestArrived {
        /// Names the request in the actions that answer it.
        waiter: WaiterId,
        /// Who asked.
        client: ClientName,
        /// The model asked for.
        model: ModelName,
        /// Where it queues.
        priority: Priority,
        /// How long it may wait before it is refused.
        max_wait: Duration,
    },
    /// A client asked for a lease.
    LeaseAsked {
        /// Names the lease in the actions that answer it until it is granted.
        waiter: WaiterId,
        /// What was asked for.
        ask: LeaseAsk,
    },
    /// A heartbeat lease's holder renewed it.
    LeaseRenewed {
        /// The lease.
        lease: LeaseId,
    },
    /// A lease's holder sent a progress note.
    LeaseNoted {
        /// The lease.
        lease: LeaseId,
        /// What the holder says now.
        note: String,
    },
    /// A lease's holder released it.
    LeaseReleased {
        /// The lease.
        lease: LeaseId,
    },
    /// An admin client revoked a lease.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "sent once the engine takes a revoke")
    )]
    LeaseRevoked {
        /// The lease.
        lease: LeaseId,
        /// The admin client.
        by: ClientName,
        /// The reason it gave, if any.
        note: Option<String>,
    },
    /// A connection lease's stream broke without a release.
    HolderDetached {
        /// The lease.
        lease: LeaseId,
    },
    /// A connection lease's holder attached to it again.
    HolderAttached {
        /// The lease.
        lease: LeaseId,
    },
    /// A waiting request's or lease's client went away.
    WaiterGone {
        /// The request or lease.
        waiter: WaiterId,
    },
    /// A forwarded request's response ended.
    RequestFinished {
        /// The model that served it.
        model: ModelName,
        /// Who sent it.
        client: ClientName,
    },
    /// A load finished and the model is ready.
    Loaded {
        /// The model.
        model: ModelName,
    },
    /// A load failed or was not ready in time.
    LoadFailed {
        /// The model.
        model: ModelName,
        /// What the backend said.
        error: String,
    },
    /// An unload finished.
    Unloaded {
        /// The model.
        model: ModelName,
    },
    /// The process serving the model exited.
    BackendExited {
        /// The model.
        model: ModelName,
    },
    /// Something other than the dog loaded a model.
    StrayFound {
        /// The model, or a stand-in's name.
        model: ModelName,
        /// What it counts for.
        footprint: Footprint,
        /// The backend it was found on.
        backend: Backend,
    },
    /// Time passed.
    Tick,
}

/// What the engine is to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start loading the model.
    Load(ModelName),
    /// Start unloading the model.
    Unload(ModelName),
    /// Send the request on to its model.
    Forward {
        /// The request.
        waiter: WaiterId,
        /// The model to send it to.
        model: ModelName,
        /// Who sent it.
        client: ClientName,
    },
    /// Tell the lease's holder it holds its model.
    Grant {
        /// The waiter that asked for the lease.
        waiter: WaiterId,
        /// The lease.
        lease: LeaseId,
    },
    /// Answer the waiter that it is busy, and why.
    Refuse {
        /// The request or lease.
        waiter: WaiterId,
        /// Why, and when to try again.
        refusal: Refusal,
    },
    /// Answer the waiter with an error.
    Fail {
        /// The request or lease.
        waiter: WaiterId,
        /// What went wrong.
        error: String,
    },
    /// Tell the waiter why it still waits.
    Waiting {
        /// The request or lease.
        waiter: WaiterId,
        /// Why it waits.
        reason: Reason,
        /// When it should be served, when that can be said.
        estimate: Option<Moment>,
    },
    /// Tell the lease's holder that it ended.
    LeaseEnded {
        /// The lease.
        lease: LeaseId,
        /// How it ended.
        why: Ended,
    },
    /// Save the leases, since one was granted or ended.
    Persist,
}

impl Action {
    /// Unloads free room before loads take it, and answers come last
    pub(super) fn rank(&self) -> u8 {
        match self {
            Self::Unload(_) => 0,
            Self::Load(_) => 1,
            Self::Forward { .. } | Self::Grant { .. } => 2,
            Self::Fail { .. } | Self::Refuse { .. } => 3,
            Self::Waiting { .. } => 4,
            Self::LeaseEnded { .. } => 5,
            Self::Persist => 6,
        }
    }
}
