//! What `run` does with its lease's stream while the command runs.

use std::{io::Write, time::Duration};

use reqwest::StatusCode;
use tokio::time::{Instant, sleep};

use super::stream::{Event, Next, Rejected, Stream, open};
use crate::cli::{Link, say};

/// What the holder's stream is doing while the command runs
pub(super) enum Watch {
    Streaming(Stream),
    /// The stream broke; attach again until `until`.
    Reattaching {
        until: Instant,
    },
    /// The lease is gone, and there is nothing to attach to or release.
    Gone,
}

impl Watch {
    /// Takes one step, then returns; `Gone` never does
    ///
    /// # Cancellation safety
    /// Safe: a step cut short by the command's exit leaves the state as it was.
    pub(super) async fn step(
        &mut self,
        client: &reqwest::Client,
        link: &Link,
        id: &str,
        reconnect: Duration,
        err: &mut impl Write,
    ) {
        match self {
            Self::Streaming(stream) => match stream.next(link.silence).await {
                Next::Event(Event::Ended { reason, idle_for }) => {
                    match (reason.as_str(), idle_for) {
                        ("idle", Some(idle_for)) => say(
                            err,
                            format_args!(
                                "the lease was released after {idle_for} without use; letting the command finish"
                            ),
                        ),
                        _ => say(
                            err,
                            format_args!("the lease ended ({reason}); letting the command finish"),
                        ),
                    }
                    *self = Self::Gone;
                }
                Next::Event(_) => {}
                Next::Broken => {
                    say(
                        err,
                        "the connection to the dog broke; trying to attach again",
                    );
                    *self = Self::Reattaching {
                        until: Instant::now() + reconnect,
                    };
                }
            },
            Self::Reattaching { until } => {
                sleep(link.retry).await;
                if Instant::now() >= *until {
                    say(
                        err,
                        "the reconnect time ran out; letting the command finish",
                    );
                    *self = Self::Gone;
                    return;
                }
                match open(client, link, &format!("/paddock/leases/{id}/attach"), None).await {
                    Ok(stream) => *self = Self::Streaming(stream),
                    Err(Rejected::Status(
                        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED,
                        _,
                    )) => {
                        say(err, "the lease is gone; letting the command finish");
                        *self = Self::Gone;
                    }
                    Err(_) => {}
                }
            }
            Self::Gone => core::future::pending().await,
        }
    }

    pub(super) fn gone(&self) -> bool {
        matches!(self, Self::Gone)
    }
}
