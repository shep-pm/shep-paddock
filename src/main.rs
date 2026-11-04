//! A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint.
//!
//! Started by the shepherd with no arguments, this is the dog. Started with arguments, it is the
//! command line: `shep paddock run` and `shep paddock status`.

mod backend;
mod book;
mod cli;
mod config;
mod config_watch;
mod discover;
mod dog;
mod engine;
mod footprint;
mod http;
mod meter;
mod metrics;
mod outbound;
mod rejoin;
mod saved;
mod shepherd;
mod survey;
#[cfg(test)]
mod test_support;

use shep_client::dogs::{Permission, Probe};

/// What the dog asks the shepherd for, as `shep adopt` shows it to the operator. The shepherd
/// refuses any request outside these, so a new request in `shepherd.rs` adds a line here.
const PERMISSIONS: [(&str, &str); 5] = [
    ("flock.read", "lists the flock and describes its sheep"),
    ("flock.control", "stops and restarts the sheep it leases"),
    (
        "flock.edit",
        "sets a sheep's environment, script and args before it starts it",
    ),
    (
        "events:process.",
        "watches sheep start, exit and come online",
    ),
    (
        "events:config.dog.",
        "watches the dogs' sections for changes",
    ),
];

fn main() -> std::process::ExitCode {
    // First, before this process opens a socket or a file: `shep adopt` asks the binary
    // `--version` and then `--schema` and reads one line of each.
    PERMISSIONS
        .iter()
        .fold(
            Probe::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")).ask_for_channel(),
            |probe, (name, reason)| probe.permission(Permission::new(*name, true, *reason)),
        )
        .answer::<config::section::Section>();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return dog::main();
    }
    match cli::parse(args.iter().map(String::as_str)) {
        Ok(command) => cli::main(command),
        Err(usage) => {
            eprintln!("{usage}");
            std::process::ExitCode::from(cli::USAGE_EXIT)
        }
    }
}
