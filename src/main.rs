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
mod outbound;
mod saved;
mod shepherd;
mod survey;
#[cfg(test)]
mod test_support;

fn main() -> std::process::ExitCode {
    // First, before this process opens a socket or a file: `shep adopt` asks the binary
    // `--version` and then `--schema` and reads one line of each.
    shep_client::dogs::probe::<config::section::Section>(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
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
