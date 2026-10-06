//! The command line: `run` holds a lease around a command, `status` prints the book.

use core::fmt;
use std::{io::Write, process::ExitCode, time::Duration};

use shep_client::shep_core::values::UpDuration;

mod run;
mod status;

/// Where the dog listens unless `$PADDOCK_URL` says otherwise
const DEFAULT_URL: &str = "http://127.0.0.1:8700";

/// How long a lease's stream may stay silent before it counts as broken: three of the dog's
/// 15 s heartbeats
const STREAM_SILENCE: Duration = Duration::from_secs(45);

/// How long to wait between attempts to attach to a lease again
const REATTACH: Duration = Duration::from_secs(2);

/// The exit code for a command line that cannot be carried out, as in `sysexits.h`
pub(crate) const USAGE_EXIT: u8 = 2;

#[cfg(test)]
mod tests;

/// Everything the command line accepts, printed when it is handed anything else
pub(crate) const USAGE: &str = "\
Usage:
  shep paddock run --model <model> [--expected <duration>] [--note <text>]
                   [--interactive] -- <command> [args...]
                          Take a lease on a model, run the command while it
                          is held, and release it when the command exits.
  shep paddock status     Print the models, leases and waiters.

$PADDOCK_KEY is the client key. It stays in the command's environment, so a
command that sends requests through the dog can use it. $PADDOCK_URL is the
dog's address and defaults to http://127.0.0.1:8700. A TERM or HUP sent to
`run` goes on to the command, and the lease is released once it exits.";

/// What the command line asked for
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Hold a lease around a command.
    Run(RunArgs),
    /// Print the status.
    Status,
}

/// The arguments of `run`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunArgs {
    /// The model to lease.
    pub model: String,
    /// How long the command is expected to take, for estimates.
    pub expected: Option<String>,
    /// What the command is for.
    pub note: Option<String>,
    /// Queue ahead of batch work.
    pub interactive: bool,
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
/// [`Usage`] for an unknown command or flag, a flag given twice, a flag missing its value, a missing `--model`,
/// an `--expected` that is not a duration such as `8h`, or no command after `--`.
pub(crate) fn parse<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Command, Usage> {
    let mut args = args.into_iter();
    match args.next() {
        Some("status") => match args.next() {
            None => Ok(Command::Status),
            Some(_) => Err(Usage("status takes no arguments.".to_owned())),
        },
        Some("run") => parse_run(args).map(Command::Run),
        Some(other) => Err(Usage(format!("{other} is not a command."))),
        None => Err(Usage("Say what to do.".to_owned())),
    }
}

fn parse_run<'a>(mut args: impl Iterator<Item = &'a str>) -> Result<RunArgs, Usage> {
    let mut model = None;
    let mut expected = None;
    let mut note = None;
    let mut interactive = None;
    let command = loop {
        let Some(arg) = args.next() else {
            return Err(Usage("the command goes after --.".to_owned()));
        };
        match arg {
            "--" => break args.map(str::to_owned).collect::<Vec<_>>(),
            "--interactive" => once(&mut interactive, arg, ())?,
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
            other => return Err(Usage(format!("run does not understand {other}."))),
        }
    };
    let model = model.ok_or_else(|| Usage("--model is required.".to_owned()))?;
    if command.is_empty() {
        return Err(Usage("there is no command after --.".to_owned()));
    }
    Ok(RunArgs {
        model,
        expected,
        note,
        interactive: interactive.is_some(),
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

/// Where the dog is and the key to speak to it with
#[derive(Clone)]
pub(crate) struct Link {
    /// The dog's address, without a trailing slash.
    pub url: String,
    /// The client key.
    pub key: String,
    /// How long to wait between attempts to attach to a lease again.
    pub retry: Duration,
    /// How long a lease's stream may stay silent before it counts as broken.
    pub silence: Duration,
}

impl Link {
    /// The link `$PADDOCK_URL` and `$PADDOCK_KEY` name, or `None` without a key
    pub(crate) fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let key = env("PADDOCK_KEY").filter(|key| !key.is_empty())?;
        let url = env("PADDOCK_URL")
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_owned());
        Some(Self {
            url: url.trim_end_matches('/').to_owned(),
            key,
            retry: REATTACH,
            silence: STREAM_SILENCE,
        })
    }

    /// A request to `path` on the dog, carrying the key
    fn request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.key)
    }
}

// The key is left out, so a `{:?}` in a log line cannot leak it.
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
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
    let Some(link) = Link::from_env(env) else {
        let _ = writeln!(
            err,
            "paddock: $PADDOCK_KEY is not set. It is this client's key."
        );
        return USAGE_EXIT;
    };
    match command {
        Command::Run(args) => run::run(&link, &args, err, signals).await,
        Command::Status => status::status(&link, out, err).await,
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
