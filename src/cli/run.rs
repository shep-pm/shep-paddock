//! `shep paddock run`: hold a lease for as long as a command runs.
//!
//! The lease is held by an open connection. If the connection breaks the command carries on
//! and the stream is attached again until the dog's `reconnect` time runs out. A bare lease's
//! command runs in its own process group, which is stopped if the lease is revoked: TERM, then
//! KILL once `--grace` has passed. The lease is held until the whole group is gone.

use std::{
    io::{self, Write},
    process::ExitStatus,
    time::Duration,
};

use reqwest::Method;
use serde_json::{Map, Value, json};
use tokio::{
    process::{Child, Command},
    sync::mpsc::UnboundedReceiver,
    time::{sleep, timeout},
};

use self::stream::{Event, Next, Stream, open};
use super::{Forward, Link, RunArgs, say};
use crate::outbound::http_client;
use watch::{Held, Watch, group_alive, send_signal};

mod stream;
mod watch;

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

// How often a bare run whose command has exited asks whether its group is gone. Each ask runs
// kill(1) once, so ten a second costs little.
const GROUP_POLL: Duration = Duration::from_millis(100);

fn take_body(args: &RunArgs) -> Value {
    let mut body = Map::new();
    match &args.model {
        Some(model) => {
            body.insert("model".to_owned(), json!(model));
        }
        None => {
            let mut footprint = Map::new();
            if let Some(vram) = &args.vram {
                footprint.insert("vram".to_owned(), json!(vram));
            }
            if let Some(ram) = &args.ram {
                footprint.insert("ram".to_owned(), json!(ram));
            }
            body.insert("footprint".to_owned(), Value::Object(footprint));
            // The command is this process's child, so every process it starts descends from here.
            body.insert("pid".to_owned(), json!(std::process::id()));
        }
    }
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
    if let Some(idle) = &args.release_if_idle {
        body.insert("release_if_idle".to_owned(), json!(idle));
    }
    if args.reclaimable {
        body.insert("reclaimable".to_owned(), json!(true));
    }
    Value::Object(body)
}

/// What the lease is on, as `run` says it: the model, or the memory a bare lease asks for
fn leased(args: &RunArgs) -> String {
    match &args.model {
        Some(model) => model.clone(),
        None => format!(
            "{} VRAM, {} RAM",
            args.vram.as_deref().unwrap_or("no"),
            args.ram.as_deref().unwrap_or("0")
        ),
    }
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
            Next::Event(Event::Ended { reason, .. }) => {
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

/// Starts the command, a bare lease's in a process group of its own
fn spawn(args: &RunArgs, id: &str) -> io::Result<Child> {
    let (program, rest) = args
        .command
        .split_first()
        .ok_or(io::ErrorKind::InvalidInput)?;
    let mut command = Command::new(program);
    command.args(rest).env("PADDOCK_LEASE", id);
    #[cfg(unix)]
    if args.model.is_none() {
        command.process_group(0);
    }
    command.spawn()
}

/// Sends `signal` on to the command, or to a bare lease's whole process group
///
/// Through `kill(1)`, since the crate has no unsafe and no libc. A command that has already
/// gone is not an error.
async fn forward(held: &Held<'_>, signal: Forward, err: &mut impl Write) {
    let Some(target) = held.target() else { return };
    let name = match signal {
        // The terminal has sent it to the command already, unless it is in a group of its own.
        Forward::Interrupt if !held.bare => return,
        Forward::Interrupt => "INT",
        Forward::Terminate => "TERM",
        Forward::Hangup => "HUP",
    };
    send_signal(&target, name, err).await;
}

/// Holds a bare lease, once its command has exited, until its process group is gone or killed
///
/// Signals and the stream are handled as while the command ran, so a revoke still stops the
/// group.
async fn outlive(
    watch: &mut Watch,
    client: &reqwest::Client,
    link: &Link,
    held: &Held<'_>,
    err: &mut impl Write,
    signals: &mut UnboundedReceiver<Forward>,
) {
    let Some(pgid) = held.pid.filter(|_| held.bare) else {
        return;
    };
    while !watch.killed() && group_alive(pgid).await {
        tokio::select! {
            () = sleep(GROUP_POLL) => {}
            Some(signal) = signals.recv() => forward(held, signal, err).await,
            () = watch.step(client, link, held, err) => {}
        }
    }
}

/// Runs the command under a lease, returning the exit code
///
/// A TERM or HUP arriving on `signals` is passed on to the command, an INT too for a bare lease,
/// and the lease is held until the command exits, and a bare lease's until its group is gone. A refused lease is [`TEMPFAIL`] and the command never starts. Otherwise the code is the
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
                format_args!("could not take the lease on {}: {rejected}", leased(args)),
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
    let held = Held {
        id: &id,
        reconnect,
        pid: child.id(),
        bare: args.model.is_none(),
        grace: args.grace,
    };
    let mut watch = Watch::Streaming(stream);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            Some(signal) = signals.recv() => forward(&held, signal, err).await,
            () = watch.step(&client, link, &held, err) => {}
        }
    };
    outlive(&mut watch, &client, link, &held, err, signals).await;
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
    if !watch.ended() {
        release(&client, link, &id, err).await;
    }
    code
}

#[cfg(test)]
mod tests;
