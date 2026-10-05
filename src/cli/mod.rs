//! The command line: `run` holds a lease around a command, `status` prints the book.

use core::fmt;
use std::{io::Write, process::ExitCode, time::Duration};

use shep_client::shep_core::values::UpDuration;

mod run;
mod status;

/// Where the dog listens unless `$PADDOCK_URL` says otherwise
const DEFAULT_URL: &str = "http://127.0.0.1:8700";

/// How long to wait between attempts to attach to a lease again
const REATTACH: Duration = Duration::from_secs(2);

/// The exit code for a command line that cannot be carried out, as in `sysexits.h`
const USAGE_EXIT: u8 = 2;

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

$PADDOCK_KEY is the client key. $PADDOCK_URL is the dog's address and
defaults to http://127.0.0.1:8700.";

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
/// [`Usage`] for an unknown command or flag, a flag missing its value, a missing `--model`,
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
    let mut interactive = false;
    let command = loop {
        let Some(arg) = args.next() else {
            return Err(Usage("the command goes after --.".to_owned()));
        };
        match arg {
            "--" => break args.map(str::to_owned).collect::<Vec<_>>(),
            "--interactive" => interactive = true,
            "--model" => model = Some(value(&mut args, arg)?.to_owned()),
            "--note" => note = Some(value(&mut args, arg)?.to_owned()),
            "--expected" => {
                let text = value(&mut args, arg)?;
                if text.parse::<UpDuration>().is_err() {
                    return Err(Usage(format!(
                        "--expected is not a duration such as 30s or 8h: {text}."
                    )));
                }
                expected = Some(text.to_owned());
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
        interactive,
        command,
    })
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

/// Runs `command` with `env` for its settings, returning the exit code
pub(crate) async fn execute(
    env: &dyn Fn(&str) -> Option<String>,
    command: Command,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let Some(link) = Link::from_env(env) else {
        let _ = writeln!(
            err,
            "paddock: $PADDOCK_KEY is not set. It is this client's key."
        );
        return USAGE_EXIT;
    };
    match command {
        Command::Run(args) => run::run(&link, &args, err).await,
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
        // A ctrl-c reaches the command too, since it shares the terminal's process group. This
        // process stays to see the command out and release the lease.
        tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
        execute(
            &env,
            command,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
        .await
    });
    ExitCode::from(code)
}
