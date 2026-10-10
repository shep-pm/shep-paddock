//! What `run` does with its lease's stream while the command runs.

use std::{io::Write, time::Duration};

use reqwest::StatusCode;
use tokio::{
    process::Command,
    time::{Instant, sleep, sleep_until},
};

use super::stream::{Event, Next, Rejected, Stream, open};
use crate::{
    cli::{Link, say},
    http::reply::rough,
};

/// Sends `SIG<name>` to `pid` through `kill(1)`, saying on `err` when it could not
///
/// Through `kill(1)`, since the crate has no unsafe and no libc. tokio keeps an exited command
/// as a zombie until `wait` returns, so its pid cannot be reused before this kill.
pub(super) async fn send_signal(pid: u32, name: &str, err: &mut impl Write) {
    let sent = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status()
        .await;
    if !sent.is_ok_and(|status| status.success()) {
        say(err, format_args!("could not pass {name} on to the command"));
    }
}

/// What `run` says when its lease ends while the command runs
fn ended_message(
    reason: &str,
    idle_for: Option<&str>,
    by: Option<&str>,
    note: Option<&str>,
    bare: bool,
) -> String {
    match (reason, idle_for) {
        ("idle", Some(idle_for)) => format!(
            "the lease was released after {idle_for} without use; letting the command finish"
        ),
        ("revoked", _) => {
            let by = by.unwrap_or("an admin client");
            let why = note.map_or_else(String::new, |note| format!(": {note}"));
            let then = if bare {
                "stopping the command"
            } else {
                "letting the command finish"
            };
            format!("the lease was revoked by {by}{why}; {then}")
        }
        _ => format!("the lease ended ({reason}); letting the command finish"),
    }
}

/// What `run` holds while the command runs
pub(super) struct Held<'a> {
    pub(super) id: &'a str,
    pub(super) reconnect: Duration,
    /// The command's pid, to stop it with.
    pub(super) pid: Option<u32>,
    /// Whether the lease is bare, so a revoke stops the command.
    pub(super) bare: bool,
    pub(super) grace: Duration,
}

/// What the holder's stream is doing while the command runs
pub(super) enum Watch {
    Streaming(Stream),
    /// The stream broke; attach again until `until`.
    Reattaching {
        until: Instant,
    },
    /// The bare lease was revoked or is gone, so the command is being stopped: `TERM` once
    /// `termed`, then `KILL` at `kill_at`, after which it is `None`. A revoked lease's stream
    /// stays open, so the dog counts the memory until `run` exits.
    Stopping {
        /// Held, never read: dropping it would close the connection.
        _stream: Option<Stream>,
        termed: bool,
        kill_at: Option<Instant>,
    },
    /// The lease is gone, and there is nothing to attach to or release.
    Gone,
}

impl Watch {
    /// Takes one step, then returns; `Gone`, and `Stopping` once the command is killed, never do
    ///
    /// # Cancellation safety
    /// Safe: a step cut short by the command's exit or a signal leaves a state the next step
    /// carries on from, at worst sending a signal twice.
    pub(super) async fn step(
        &mut self,
        client: &reqwest::Client,
        link: &Link,
        held: &Held<'_>,
        err: &mut impl Write,
    ) {
        match self {
            Self::Streaming(stream) => match stream.next(link.silence).await {
                Next::Event(Event::Ended {
                    reason,
                    idle_for,
                    by,
                    note,
                }) => {
                    let said = ended_message(
                        &reason,
                        idle_for.as_deref(),
                        by.as_deref(),
                        note.as_deref(),
                        held.bare,
                    );
                    say(err, said);
                    let stopping = reason == "revoked" && held.bare;
                    let Self::Streaming(stream) = core::mem::replace(self, Self::Gone) else {
                        return;
                    };
                    if stopping {
                        *self = Self::stopping(Some(stream), held);
                    }
                }
                Next::Event(_) => {}
                Next::Broken => {
                    say(
                        err,
                        "the connection to the dog broke; trying to attach again",
                    );
                    *self = Self::Reattaching {
                        until: Instant::now() + held.reconnect,
                    };
                }
            },
            Self::Stopping {
                termed, kill_at, ..
            } => {
                if !*termed {
                    if let Some(pid) = held.pid {
                        send_signal(pid, "TERM", err).await;
                    }
                    *termed = true;
                    return;
                }
                let Some(at) = *kill_at else {
                    return core::future::pending().await;
                };
                sleep_until(at).await;
                say(
                    err,
                    format_args!(
                        "the command did not stop within {}; killing it",
                        rough(held.grace)
                    ),
                );
                if let Some(pid) = held.pid {
                    send_signal(pid, "KILL", err).await;
                }
                *kill_at = None;
            }
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
                let path = format!("/paddock/leases/{}/attach", held.id);
                match open(client, link, &path, None).await {
                    Ok(stream) => *self = Self::Streaming(stream),
                    Err(Rejected::Status(
                        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED,
                        _,
                    )) => {
                        if held.bare {
                            say(err, "the lease is gone; stopping the command");
                            *self = Self::stopping(None, held);
                        } else {
                            say(err, "the lease is gone; letting the command finish");
                            *self = Self::Gone;
                        }
                    }
                    Err(_) => {}
                }
            }
            Self::Gone => core::future::pending().await,
        }
    }

    /// The state that stops a bare lease's command, holding `stream` open if there is one
    fn stopping(stream: Option<Stream>, held: &Held<'_>) -> Self {
        Self::Stopping {
            _stream: stream,
            termed: false,
            kill_at: Some(Instant::now() + held.grace),
        }
    }

    /// Whether the lease has ended, so there is nothing to release
    pub(super) fn ended(&self) -> bool {
        matches!(self, Self::Gone | Self::Stopping { .. })
    }
}
