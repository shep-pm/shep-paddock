//! `shep paddock run`: hold a lease for as long as a command runs.
//!
//! The lease is held by an open connection. If the connection breaks the command carries on
//! and the stream is attached again until the dog's `reconnect` time runs out.

use std::{
    io::{self, Write},
    process::ExitStatus,
    time::Duration,
};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use tokio::{
    process::{Child, Command},
    sync::mpsc::UnboundedReceiver,
    time::{Instant, sleep, timeout},
};

use self::stream::{Event, Next, Rejected, Stream, open};
use super::{Forward, Link, RunArgs};
use crate::outbound::http_client;

mod stream;

/// The exit code for a lease that was refused, or that never came: try again later
const TEMPFAIL: u8 = 75;
const FAILED: u8 = 1;
// The shell's codes for a command that was not found and one that could not be run.
const NOT_FOUND: u8 = 127;
const NOT_RUNNABLE: u8 = 126;
// A shell reports a command killed by signal `n` as 128 plus `n`.
const SIGNAL_BASE: u8 = 128;

// A release or a status answers from memory; ten seconds is a dog that is not answering.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

// How long a run leaving the queue waits for a grant that may already be on its way.
const LEAVE_GRACE: Duration = Duration::from_millis(500);

fn take_body(args: &RunArgs) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(args.model));
    let priority = if args.interactive {
        "interactive"
    } else {
        "batch"
    };
    body.insert("priority".to_owned(), json!(priority));
    body.insert("hold".to_owned(), json!("connection"));
    if let Some(expected) = &args.expected {
        body.insert("expected".to_owned(), json!(expected));
    }
    if let Some(note) = &args.note {
        body.insert("note".to_owned(), json!(note));
    }
    Value::Object(body)
}

fn say(err: &mut impl Write, what: impl core::fmt::Display) {
    let _ = writeln!(err, "paddock: {what}");
}

/// Says the run is leaving the queue, and the exit code a shell gives a process ended by `signal`
///
/// Nothing is forwarded later: the command has not started.
fn left_queue(signal: Forward, err: &mut impl Write) -> u8 {
    say(
        err,
        "interrupted before the lease was granted; leaving the queue",
    );
    SIGNAL_BASE
        + match signal {
            Forward::Hangup => 1,
            Forward::Interrupt => 2,
            Forward::Terminate => 15,
        }
}

/// The lease id of a grant that was already on its way when the run decided to leave
async fn granted_meanwhile(stream: &mut Stream, silence: Duration) -> Option<String> {
    let waiting = async {
        loop {
            match stream.next(silence).await {
                Next::Event(Event::Granted { id, .. }) => return Some(id),
                Next::Event(Event::Queued { .. } | Event::Heartbeat | Event::Unknown) => {}
                Next::Event(_) | Next::Broken => return None,
            }
        }
    };
    timeout(LEAVE_GRACE, waiting).await.ok().flatten()
}

/// Reads the stream up to the grant, telling the holder why it waits
///
/// Returns the lease id and the dog's reconnect time, or the exit code for a lease that did not
/// come.
async fn grant(
    stream: &mut Stream,
    client: &reqwest::Client,
    link: &Link,
    err: &mut impl Write,
    signals: &mut UnboundedReceiver<Forward>,
) -> Result<(String, Duration), u8> {
    loop {
        let next = tokio::select! {
            next = stream.next(link.silence) => next,
            Some(signal) = signals.recv() => {
                let code = left_queue(signal, err);
                if let Some(id) = granted_meanwhile(stream, link.silence).await {
                    release(client, link, &id, err).await;
                }
                return Err(code);
            }
        };
        match next {
            Next::Event(Event::Queued { reason }) => say(err, format_args!("waiting: {reason}")),
            Next::Event(Event::Granted { id, reconnect }) => return Ok((id, reconnect)),
            Next::Event(Event::Refused {
                reason,
                expected_until,
            }) => {
                match expected_until {
                    Some(until) => say(
                        err,
                        format_args!("refused: {reason} (expected until {until})"),
                    ),
                    None => say(err, format_args!("refused: {reason}")),
                }
                return Err(TEMPFAIL);
            }
            Next::Event(Event::Failed { reason }) => {
                say(err, format_args!("the model could not be loaded: {reason}"));
                return Err(FAILED);
            }
            Next::Event(Event::Ended { reason }) => {
                say(
                    err,
                    format_args!("the lease ended before it was granted ({reason})"),
                );
                return Err(FAILED);
            }
            Next::Event(Event::Heartbeat | Event::Unknown) => {}
            Next::Broken => {
                say(err, "the connection broke before the lease was granted");
                return Err(TEMPFAIL);
            }
        }
    }
}

/// What the holder's stream is doing while the command runs
enum Watch {
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
    async fn step(
        &mut self,
        client: &reqwest::Client,
        link: &Link,
        id: &str,
        reconnect: Duration,
        err: &mut impl Write,
    ) {
        match self {
            Self::Streaming(stream) => match stream.next(link.silence).await {
                Next::Event(Event::Ended { reason }) => {
                    say(
                        err,
                        format_args!("the lease ended ({reason}); letting the command finish"),
                    );
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

    fn gone(&self) -> bool {
        matches!(self, Self::Gone)
    }
}

async fn release(client: &reqwest::Client, link: &Link, id: &str, err: &mut impl Write) {
    let sent = link
        .request(client, Method::DELETE, &format!("/paddock/leases/{id}"))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await;
    match sent {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => say(
            err,
            format_args!(
                "releasing the lease: the dog answered {}",
                response.status()
            ),
        ),
        Err(failure) => say(err, format_args!("releasing the lease failed: {failure}")),
    }
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => u8::try_from(code).unwrap_or(FAILED),
        (None, Some(signal)) => {
            u8::try_from(signal).map_or(FAILED, |n| SIGNAL_BASE.saturating_add(n))
        }
        (None, None) => FAILED,
    }
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> u8 {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(FAILED)
}

fn spawn(args: &RunArgs, id: &str) -> io::Result<Child> {
    let (program, rest) = args
        .command
        .split_first()
        .ok_or(io::ErrorKind::InvalidInput)?;
    Command::new(program)
        .args(rest)
        .env("PADDOCK_LEASE", id)
        .spawn()
}

/// Sends `signal` on to the command
///
/// Through `kill(1)`, since the crate has no unsafe and no libc. A command that has already
/// gone is not an error.
async fn forward(pid: Option<u32>, signal: Forward, err: &mut impl Write) {
    let Some(pid) = pid else { return };
    let name = match signal {
        // The terminal has sent it to the command already.
        Forward::Interrupt => return,
        Forward::Terminate => "TERM",
        Forward::Hangup => "HUP",
    };
    // tokio keeps an exited command as a zombie until `wait` returns, so its
    // pid cannot be reused before this kill.
    let sent = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status()
        .await;
    if !sent.is_ok_and(|status| status.success()) {
        say(err, format_args!("could not pass {name} on to the command"));
    }
}

/// Runs the command under a lease, returning the exit code
///
/// A TERM or HUP arriving on `signals` is passed on to the command, and the lease is held until
/// the command exits. A refused lease is [`TEMPFAIL`] and the command never starts. Otherwise the code is the
/// command's own, or 128 plus the signal that ended it.
pub(crate) async fn run(
    link: &Link,
    args: &RunArgs,
    err: &mut impl Write,
    signals: &mut UnboundedReceiver<Forward>,
) -> u8 {
    let client = http_client();
    let taking = open(&client, link, "/paddock/leases", Some(take_body(args)));
    let opened = tokio::select! {
        opened = taking => opened,
        Some(signal) = signals.recv() => return left_queue(signal, err),
    };
    let mut stream = match opened {
        Ok(stream) => stream,
        Err(rejected) => {
            say(
                err,
                format_args!("could not take the lease on {}: {rejected}", args.model),
            );
            return FAILED;
        }
    };
    let (id, reconnect) = match grant(&mut stream, &client, link, err, signals).await {
        Ok(granted) => granted,
        Err(code) => return code,
    };
    let mut child = match spawn(args, &id) {
        Ok(child) => child,
        Err(failure) => {
            say(
                err,
                format_args!("cannot run {}: {failure}", args.command.join(" ")),
            );
            release(&client, link, &id, err).await;
            return if failure.kind() == io::ErrorKind::NotFound {
                NOT_FOUND
            } else {
                NOT_RUNNABLE
            };
        }
    };
    let pid = child.id();
    let mut watch = Watch::Streaming(stream);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            Some(signal) = signals.recv() => forward(pid, signal, err).await,
            () = watch.step(&client, link, &id, reconnect, err) => {}
        }
    };
    let code = match status {
        Ok(status) => exit_code(status),
        Err(failure) => {
            say(
                err,
                format_args!("waiting for the command failed: {failure}"),
            );
            FAILED
        }
    };
    if !watch.gone() {
        release(&client, link, &id, err).await;
    }
    code
}

#[cfg(test)]
mod tests;
