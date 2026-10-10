//! The command line: `run` holds a lease around a command, `note` marks it in use, `revoke` ends a lease, `status` prints the book.

use core::fmt;
use std::{io::Write, process::ExitCode, time::Duration};

use shep_client::shep_core::values::{MemSize, UpDuration};

mod link;
mod note;
mod revoke;
mod run;
mod status;

pub(crate) use link::Link;
use link::STORED_KEY;

/// How long a revoked bare lease's command has between `TERM` and `KILL`, unless `--grace` says:
/// the spec's default
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(30);

/// The exit code for a command line that cannot be carried out, as in `sysexits.h`
pub(crate) const USAGE_EXIT: u8 = 2;

#[cfg(test)]
mod tests;

/// Everything the command line accepts, printed when it is handed anything else
pub(crate) const USAGE: &str = "\
Usage:
  shep paddock run --model <model> [--expected <duration>] [--note <text>]
                   [--interactive] [--reclaimable] [--release-if-idle <duration>]
                   -- <command> [args...]
                          Take a lease on a model, run the command while it
                          is held, and release it when the command exits.
  shep paddock run [--vram <size|all>] [--ram <size>] [--grace <duration>]
                   [--expected <duration>] [--note <text>] [--interactive]
                   -- <command> [args...]
                          Take a lease on memory for a command that runs its
                          own GPU code, naming --vram, --ram or both. If it
                          is revoked, the command gets TERM, then KILL once
                          --grace (30s) has passed. The command runs in its
                          own process group, so it should not read the
                          terminal.
  shep paddock note <text>
                          Tell the dog the lease in $PADDOCK_LEASE is still in
                          use. `run` sets $PADDOCK_LEASE for its command.
  shep paddock revoke <id> [--reason <text>]
                          End a lease, such as one left running, unless
                          another client that is protected holds it.
                          $PADDOCK_KEY must be an admin client's.
  shep paddock status     Print the models, leases and waiters.

$PADDOCK_KEY is the client key. Unset, the key is the PADDOCK_KEY secret in
shep's store, set with `shep secret set PADDOCK_KEY --stdin`. A key from the
environment stays in the command's environment, so a command that sends
requests through the dog can use it. $PADDOCK_URL is the dog's address and
defaults to http://127.0.0.1:8700. A TERM or HUP sent to `run` goes on to the
command, and the lease is released once it exits.";

/// What the command line asked for
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Hold a lease around a command.
    Run(RunArgs),
    /// Print the status.
    Status,
    /// Send a progress note for the lease in `$PADDOCK_LEASE`.
    Note(String),
    /// End a lease, as an admin client.
    Revoke {
        /// The lease's id, such as `L12`.
        id: String,
        /// Why, for the holder and the dog's log.
        reason: Option<String>,
    },
}

/// The arguments of `run`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunArgs {
    /// The model to lease, or `None` for a bare lease.
    pub model: Option<String>,
    /// A bare lease's VRAM: a size in shep's grammar, or `all`.
    pub vram: Option<String>,
    /// A bare lease's RAM: a size in shep's grammar.
    pub ram: Option<String>,
    /// How long a revoked bare lease's command has between `TERM` and `KILL`.
    pub grace: Duration,
    /// How long the command is expected to take, for estimates.
    pub expected: Option<String>,
    /// What the command is for.
    pub note: Option<String>,
    /// Queue ahead of batch work.
    pub interactive: bool,
    /// Let a waiter take the model from this lease without waiting for it to end.
    pub reclaimable: bool,
    /// End the lease after this long without use.
    pub release_if_idle: Option<String>,
    /// The program and its arguments.
    pub command: Vec<String>,
}

/// An argument the command line does not accept, and why
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Usage(String);

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

impl core::error::Error for Usage {}

/// Reads the arguments after the program name
///
/// # Errors
/// [`Usage`] for an unknown command or flag, a flag given twice, a flag missing
/// its value, a missing `--model`, an `--expected` or `--release-if-idle` that is not a
/// duration such as `8h` (or is zero, for `--release-if-idle`), a `--vram`, `--ram` or
/// `--grace` outside shep's grammar, `--model` with `--vram` or `--ram`, neither, or a flag of
/// one kind of lease on the other, a `note` without exactly one argument, a `revoke` with no id, two ids, or a flag other than
/// `--reason`, or no command after
/// `--`.
pub(crate) fn parse<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Command, Usage> {
    let mut args = args.into_iter();
    match args.next() {
        Some("status") => match args.next() {
            None => Ok(Command::Status),
            Some(_) => Err(Usage("status takes no arguments.".to_owned())),
        },
        Some("run") => parse_run(args).map(Command::Run),
        Some("note") => match (args.next(), args.next()) {
            (Some(text), None) => Ok(Command::Note(text.to_owned())),
            _ => Err(Usage(
                "note takes the text to send, as one argument.".to_owned(),
            )),
        },
        Some("revoke") => parse_revoke(args),
        Some(other) => Err(Usage(format!("{other} is not a command."))),
        None => Err(Usage("Say what to do.".to_owned())),
    }
}

fn parse_revoke<'a>(mut args: impl Iterator<Item = &'a str>) -> Result<Command, Usage> {
    let mut id = None;
    let mut reason = None;
    while let Some(arg) = args.next() {
        match arg {
            "--reason" => once(&mut reason, arg, value(&mut args, arg)?.to_owned())?,
            flag if flag.starts_with("--") => {
                return Err(Usage(format!("revoke does not understand {flag}.")));
            }
            text => once(&mut id, "the lease id", text.to_owned())?,
        }
    }
    let id = id
        .ok_or_else(|| Usage("revoke takes the id of the lease to end, such as L12.".to_owned()))?;
    Ok(Command::Revoke { id, reason })
}

fn parse_run<'a>(mut args: impl Iterator<Item = &'a str>) -> Result<RunArgs, Usage> {
    let mut model = None;
    let mut expected = None;
    let mut note = None;
    let mut interactive = None;
    let mut reclaimable = None;
    let mut release_if_idle = None;
    let mut vram = None;
    let mut ram = None;
    let mut grace = None;
    let command = loop {
        let Some(arg) = args.next() else {
            return Err(Usage("the command goes after --.".to_owned()));
        };
        match arg {
            "--" => break args.map(str::to_owned).collect::<Vec<_>>(),
            "--interactive" => once(&mut interactive, arg, ())?,
            "--reclaimable" => once(&mut reclaimable, arg, ())?,
            "--model" => once(&mut model, arg, value(&mut args, arg)?.to_owned())?,
            "--note" => once(&mut note, arg, value(&mut args, arg)?.to_owned())?,
            "--expected" => {
                let text = value(&mut args, arg)?;
                if text.parse::<UpDuration>().is_err() {
                    return Err(Usage(format!(
                        "--expected is not a duration such as 30s or 8h: {text}."
                    )));
                }
                once(&mut expected, arg, text.to_owned())?;
            }
            "--release-if-idle" => {
                let text = value(&mut args, arg)?;
                match text.parse::<UpDuration>() {
                    Err(_) => {
                        return Err(Usage(format!(
                            "--release-if-idle is not a duration such as 30s or 8h: {text}."
                        )));
                    }
                    Ok(parsed) if parsed.as_duration().is_zero() => {
                        return Err(Usage(format!(
                            "--release-if-idle must be more than 0: {text}."
                        )));
                    }
                    Ok(_) => {}
                }
                once(&mut release_if_idle, arg, text.to_owned())?;
            }
            "--vram" => {
                let text = value(&mut args, arg)?;
                if text != "all" && text.parse::<MemSize>().is_err() {
                    return Err(Usage(format!(
                        "--vram is not a size such as 12G, or all: {text}."
                    )));
                }
                once(&mut vram, arg, text.to_owned())?;
            }
            "--ram" => {
                let text = value(&mut args, arg)?;
                if text.parse::<MemSize>().is_err() {
                    return Err(Usage(format!("--ram is not a size such as 4G: {text}.")));
                }
                once(&mut ram, arg, text.to_owned())?;
            }
            "--grace" => {
                let text = value(&mut args, arg)?;
                let Ok(parsed) = text.parse::<UpDuration>() else {
                    return Err(Usage(format!(
                        "--grace is not a duration such as 30s or 2m: {text}."
                    )));
                };
                once(&mut grace, arg, parsed.as_duration())?;
            }
            other => return Err(Usage(format!("run does not understand {other}."))),
        }
    };
    let bare = vram.is_some() || ram.is_some();
    if model.is_some() && bare {
        return Err(Usage(
            "--model and --vram or --ram are exclusive.".to_owned(),
        ));
    }
    if model.is_none() && !bare {
        return Err(Usage(
            "--model is required, or --vram, --ram or both for a bare lease.".to_owned(),
        ));
    }
    let model_only = |flag: &str| {
        Usage(format!(
            "{flag} is for a model lease; a bare lease is always held."
        ))
    };
    if bare && reclaimable.is_some() {
        return Err(model_only("--reclaimable"));
    }
    if bare && release_if_idle.is_some() {
        return Err(model_only("--release-if-idle"));
    }
    if !bare && grace.is_some() {
        return Err(Usage("--grace is for a bare lease.".to_owned()));
    }
    if command.is_empty() {
        return Err(Usage("there is no command after --.".to_owned()));
    }
    Ok(RunArgs {
        model,
        vram,
        ram,
        grace: grace.unwrap_or(STOP_GRACE),
        expected,
        note,
        interactive: interactive.is_some(),
        reclaimable: reclaimable.is_some(),
        release_if_idle,
        command,
    })
}

/// Stores a flag's `value`, which a second use of the flag would silently replace
fn once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), Usage> {
    if slot.is_some() {
        return Err(Usage(format!("{flag} given more than once.")));
    }
    *slot = Some(value);
    Ok(())
}

/// The value after a flag, which `--` is not
fn value<'a>(args: &mut impl Iterator<Item = &'a str>, flag: &str) -> Result<&'a str, Usage> {
    match args.next() {
        Some("--") | None => Err(Usage(format!("{flag} needs a value."))),
        Some(value) => Ok(value),
    }
}

/// `text` with every control and bidirectional-control character, ANSI escapes included, written out as `\u{..}`
///
/// Text another client supplied, or the dog passed on, must not drive the maintainer's terminal.
pub(crate) fn plain(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() || is_bidi(c) {
            clean.extend(c.escape_unicode());
        } else {
            clean.push(c);
        }
    }
    clean
}

/// A character that reorders the text around it, so a cell can show other than it holds
///
/// Unicode's whole `Bidi_Control` set.
fn is_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Prints a `paddock:` line to `err`, with control characters escaped
fn say(err: &mut impl Write, what: impl fmt::Display) {
    let _ = writeln!(err, "paddock: {}", plain(&what.to_string()));
}

/// Says the dog at `link` cannot be reached, with no credential its url carries
fn unreachable(err: &mut impl Write, link: &Link, failure: reqwest::Error) {
    let url = crate::config::redacted(&link.url);
    let failure = failure.without_url();
    say(
        err,
        format_args!("cannot reach the dog at {url}: {failure}"),
    );
}

/// A signal sent to `run`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Forward {
    /// `SIGINT`, which the terminal sends to the command as well.
    Interrupt,
    /// `SIGTERM`.
    Terminate,
    /// `SIGHUP`.
    Hangup,
}

/// The INT, TERM and HUP sent to this process, as they arrive
///
/// Needs a runtime. The handlers are installed before this returns, so a signal sent after it
/// is not lost.
///
/// # Errors
/// The I/O error from installing a signal handler.
#[cfg(unix)]
pub(crate) fn forwarded_signals() -> std::io::Result<tokio::sync::mpsc::UnboundedReceiver<Forward>>
{
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let forward = tokio::select! {
                Some(()) = interrupt.recv() => Forward::Interrupt,
                Some(()) = terminate.recv() => Forward::Terminate,
                Some(()) = hangup.recv() => Forward::Hangup,
                else => return,
            };
            if sender.send(forward).is_err() {
                return;
            }
        }
    });
    Ok(receiver)
}

/// No signals arrive where there is no `SIGTERM`.
///
/// # Errors
/// Never.
#[cfg(not(unix))]
pub(crate) fn forwarded_signals() -> std::io::Result<tokio::sync::mpsc::UnboundedReceiver<Forward>>
{
    Ok(tokio::sync::mpsc::unbounded_channel().1)
}

/// Runs `command` with `env` for its settings, returning the exit code
pub(crate) async fn execute(
    env: &dyn Fn(&str) -> Option<String>,
    command: Command,
    out: &mut impl Write,
    err: &mut impl Write,
    signals: &mut tokio::sync::mpsc::UnboundedReceiver<Forward>,
) -> u8 {
    let link = match Link::from_env(env) {
        Ok(Some(link)) => link,
        Ok(None) => {
            let _ = writeln!(
                err,
                "paddock: $PADDOCK_KEY is not set, and shep's secret store holds no {STORED_KEY}. \
                 Either is this client's key."
            );
            return USAGE_EXIT;
        }
        Err(store) => {
            let _ = writeln!(err, "paddock: cannot read shep's secret store: {store}");
            return 1;
        }
    };
    match command {
        Command::Run(args) => run::run(&link, &args, err, signals).await,
        Command::Revoke { id, reason } => revoke::revoke(&link, &id, reason.as_deref(), err).await,
        Command::Status => status::status(&link, out, err).await,
        Command::Note(text) => note::note(&link, env("PADDOCK_LEASE"), &text, err).await,
    }
}

/// Runs the command line on a runtime of its own
pub(crate) fn main(command: Command) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("paddock: cannot start a runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let env = |name: &str| std::env::var(name).ok();
    let code = runtime.block_on(async {
        let mut signals = match forwarded_signals() {
            Ok(signals) => signals,
            Err(err) => {
                eprintln!("paddock: cannot watch for signals: {err}");
                return 1;
            }
        };
        execute(
            &env,
            command,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
            &mut signals,
        )
        .await
    });
    ExitCode::from(code)
}
