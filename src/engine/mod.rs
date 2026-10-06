//! The engine: the one task that owns the [`Book`](crate::book::Book) and acts on its decisions.
//!
//! An [`EngineHandle`] sends it requests and lease commands. It runs each
//! [`Action`](crate::book::Action) against the backends, and feeds back what
//! they report, what the shepherd says happened to each sheep, and the passing
//! of time, as [`Event`](crate::book::Event)s. See [`run()`].

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use core::fmt;

use tokio::sync::{mpsc, oneshot};

use crate::{
    book::{Ended, Hold, LeaseId, Priority, Reason, Refusal, Snapshot, WaiterId},
    config::{ClientName, Config, ModelName},
    discover::Discovered,
    footprint::{Footprint, Vram},
    saved::Saved,
};

mod clock;
mod guards;
mod lease_events;
mod run;
mod state;

#[cfg(test)]
mod tests;

pub(crate) use clock::Clock;
pub(crate) use guards::InFlight;
use guards::WaiterGuard;
pub(crate) use lease_events::{LeaseEvents, LeaseSender, lease_channel};
pub(crate) use run::run;

// Room for a burst of requests to queue while the engine works through one
// event; a sender waits once it is full.
const COMMANDS: usize = 256;
const STOPPED: &str = "the engine has stopped";

/// Where the engine saves its state, and what it picks up when it starts
#[derive(Debug, Default)]
pub(crate) struct Start {
    /// Where `state.json` is written, or `None` to write nothing.
    pub state: Option<PathBuf>,
    /// What the dog saved before it last stopped.
    pub saved: Saved,
    /// What is loaded on the host now.
    pub discovered: Discovered,
}

/// How a request was answered
#[derive(Debug)]
pub(crate) enum Admission {
    /// Its model is loaded: send it on, and drop this when the response ends.
    Forward(InFlight),
    /// It is busy, and why.
    Refused(Refusal),
    /// Its model could not be loaded, or the engine has stopped.
    Failed(String),
    /// No model has that name.
    Unknown,
}

/// What a lease's holder hears
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaseEvent {
    /// It still waits.
    Waiting {
        /// Why.
        reason: Reason,
        /// When it should be granted, when that can be said.
        estimate: Option<jiff::Timestamp>,
    },
    /// It holds its model.
    Granted {
        /// The lease, for renewing, releasing and attaching again.
        lease: LeaseId,
    },
    /// It was turned away, and why.
    Refused(Refusal),
    /// Its model could not be loaded, or no model has that name.
    Failed(String),
    /// It ended.
    Ended(Ended),
}

/// Why a command naming a lease was not applied
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseRefused {
    /// No granted lease has that id: it never existed, or it ended.
    NotFound,
    /// The lease is another client's.
    NotYours,
    /// A holder's stream is already open on the lease.
    Attached,
}

impl fmt::Display for LeaseRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "no such lease",
            Self::NotYours => "the lease is another client's",
            Self::Attached => "a holder is already attached to the lease",
        })
    }
}

impl core::error::Error for LeaseRefused {}

/// What a client asks for. The engine assigns the `LeaseId` and adds the client to make a `LeaseAsk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseRequest {
    /// The model to hold.
    pub model: ModelName,
    /// Where it queues.
    pub priority: Priority,
    /// How long the holder expects to keep it, for estimates only.
    pub expected: Option<Duration>,
    /// How long it may wait before it is refused. Without one it waits on.
    pub max_wait: Option<Duration>,
    /// How its holder shows it is still alive.
    pub hold: Hold,
    /// What the holder says it is for.
    pub note: Option<String>,
}

/// What an [`EngineHandle`] asks of the engine
#[derive(Debug)]
pub(crate) enum Command {
    /// [`EngineHandle::admit`].
    Admit {
        waiter: WaiterId,
        client: ClientName,
        model: ModelName,
        priority: Priority,
        max_wait: Duration,
        reply: oneshot::Sender<Admission>,
    },
    /// [`EngineHandle::take_lease`].
    TakeLease {
        waiter: WaiterId,
        client: ClientName,
        ask: LeaseRequest,
        events: LeaseSender,
    },
    /// [`EngineHandle::attach`].
    Attach {
        client: ClientName,
        lease: LeaseId,
        events: LeaseSender,
        reply: oneshot::Sender<Result<(), LeaseRefused>>,
    },
    /// [`EngineHandle::renew`].
    Renew {
        client: ClientName,
        lease: LeaseId,
        reply: oneshot::Sender<Result<(), LeaseRefused>>,
    },
    /// [`EngineHandle::release`].
    Release {
        client: ClientName,
        lease: LeaseId,
        reply: oneshot::Sender<Result<(), LeaseRefused>>,
    },
    /// [`EngineHandle::snapshot`].
    Snapshot { reply: oneshot::Sender<Snapshot> },
    /// [`EngineHandle::reconfigure`].
    Reconfigure {
        config: Arc<Config>,
        done: oneshot::Sender<()>,
    },
    /// A waiting request's client left.
    WaiterGone { waiter: WaiterId },
    /// A forwarded request's response ended.
    Finished { model: ModelName },
}

/// The engine's ends of the channels an [`EngineHandle`] sends on
#[derive(Debug)]
pub(crate) struct Inbox {
    commands: mpsc::Receiver<Command>,
    /// What drop guards send, which cannot wait for room.
    notices: mpsc::UnboundedReceiver<Command>,
    notify: mpsc::UnboundedSender<Command>,
    clock: Clock,
}

/// A handle for [`run()`], and the inbox to give it
pub(crate) fn channel() -> (EngineHandle, Inbox) {
    let (tx, commands) = mpsc::channel(COMMANDS);
    let (notify, notices) = mpsc::unbounded_channel();
    let clock = Clock::new();
    let handle = EngineHandle {
        tx,
        notices: notify.clone(),
        waiters: Arc::new(AtomicU64::new(1)),
        clock,
    };
    let inbox = Inbox {
        commands,
        notices,
        notify,
        clock,
    };
    (handle, inbox)
}

/// How the rest of the dog reaches the engine
#[derive(Debug, Clone)]
pub(crate) struct EngineHandle {
    tx: mpsc::Sender<Command>,
    notices: mpsc::UnboundedSender<Command>,
    waiters: Arc<AtomicU64>,
    clock: Clock,
}

impl EngineHandle {
    fn waiter(&self) -> WaiterId {
        WaiterId(self.waiters.fetch_add(1, Ordering::Relaxed))
    }

    /// The engine's clock, for turning the moments in its answers into times
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Waits until a request for `model` may be sent on, or is turned away
    ///
    /// # Cancellation safety
    /// Dropping the future takes the request out of the queue.
    pub async fn admit(
        &self,
        client: ClientName,
        model: ModelName,
        priority: Priority,
        max_wait: Duration,
    ) -> Admission {
        let waiter = self.waiter();
        let guard = WaiterGuard::new(waiter, self.notices.clone());
        let (reply, answer) = oneshot::channel();
        let asked = Command::Admit {
            waiter,
            client,
            model,
            priority,
            max_wait,
            reply,
        };
        let admission = match self.tx.send(asked).await {
            Ok(()) => answer
                .await
                .unwrap_or_else(|_| Admission::Failed(STOPPED.to_owned())),
            Err(_) => Admission::Failed(STOPPED.to_owned()),
        };
        guard.answered();
        admission
    }

    /// Asks for a lease, and returns the stream of what its holder hears
    ///
    /// Dropping the receiver while it waits takes it out of the queue, and
    /// once granted detaches a connection lease's holder. The stream ends
    /// after `Refused`, `Failed` or `Ended`.
    pub async fn take_lease(&self, client: ClientName, ask: LeaseRequest) -> LeaseEvents {
        let (events, rx) = lease_channel();
        let asked = Command::TakeLease {
            waiter: self.waiter(),
            client,
            ask,
            events,
        };
        // A stopped engine drops the sender with the command, which ends the stream.
        let _ = self.tx.send(asked).await;
        rx
    }

    /// Opens a new stream on a granted lease, starting with its grant
    ///
    /// The stream ends after `Ended`.
    ///
    /// # Errors
    /// [`LeaseRefused::NotFound`] if no granted lease has that id or the
    /// engine has stopped, [`LeaseRefused::NotYours`] if it is another
    /// client's, and [`LeaseRefused::Attached`] if a stream is already open
    /// on it.
    pub async fn attach(
        &self,
        client: ClientName,
        lease: LeaseId,
    ) -> Result<LeaseEvents, LeaseRefused> {
        let (events, rx) = lease_channel();
        let (reply, answer) = oneshot::channel();
        let asked = Command::Attach {
            client,
            lease,
            events,
            reply,
        };
        self.ask(asked, answer).await.map(|()| rx)
    }

    /// Renews a heartbeat lease for another `ttl`
    ///
    /// # Errors
    /// [`LeaseRefused::NotFound`] or [`LeaseRefused::NotYours`], as for [`Self::attach`].
    pub async fn renew(&self, client: ClientName, lease: LeaseId) -> Result<(), LeaseRefused> {
        let (reply, answer) = oneshot::channel();
        let asked = Command::Renew {
            client,
            lease,
            reply,
        };
        self.ask(asked, answer).await
    }

    /// Ends a lease
    ///
    /// # Errors
    /// [`LeaseRefused::NotFound`] or [`LeaseRefused::NotYours`], as for [`Self::attach`].
    pub async fn release(&self, client: ClientName, lease: LeaseId) -> Result<(), LeaseRefused> {
        let (reply, answer) = oneshot::channel();
        let asked = Command::Release {
            client,
            lease,
            reply,
        };
        self.ask(asked, answer).await
    }

    async fn ask(
        &self,
        asked: Command,
        answer: oneshot::Receiver<Result<(), LeaseRefused>>,
    ) -> Result<(), LeaseRefused> {
        if self.tx.send(asked).await.is_err() {
            return Err(LeaseRefused::NotFound);
        }
        answer.await.unwrap_or(Err(LeaseRefused::NotFound))
    }

    /// The book as it is now, or an empty one once the engine has stopped
    pub async fn snapshot(&self) -> Snapshot {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Snapshot { reply }).await.is_err() {
            return empty_snapshot();
        }
        answer.await.unwrap_or_else(|_| empty_snapshot())
    }

    /// Applies `config` to every later decision, returning once it has
    pub async fn reconfigure(&self, config: Arc<Config>) {
        let (done, applied) = oneshot::channel();
        if self
            .tx
            .send(Command::Reconfigure { config, done })
            .await
            .is_ok()
        {
            let _ = applied.await;
        }
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        models: Vec::new(),
        leases: Vec::new(),
        waiters: Vec::new(),
        errors: Vec::new(),
        declared: Footprint {
            vram: Vram::None,
            ram: 0,
        },
    }
}
