//! The dog: connect to the shepherd, start the engine, and serve until the shepherd stops it.
//!
//! Two names, from two places, as in `shep-log-rotate`. The handshake name is `$SHEP_DOG_NAME`
//! and nothing else, because the shepherd acts on whichever dog a refused handshake names. The
//! `[<name>]` section of `dogs.toml` is the same name, or [`DEFAULT_SECTION`] for a process
//! nothing adopted, so somebody running the binary by hand still gets their settings.

use std::{process::ExitCode, sync::Arc};

use shep_client::{
    dogs::{DogIdentity, DogRuntime, Stop, resolve_paths},
    shep_core::paths::ShepPaths,
};
use tokio::{net::TcpListener, sync::watch};

use crate::{
    backend::Backends,
    config::Config,
    config_watch, discover,
    engine::{self, Start},
    http::{self, Shared, Timeouts},
    outbound::http_client,
    saved,
    shepherd::Live,
};

/// The `[<name>]` section to read when `$SHEP_DOG_NAME` is unset
const DEFAULT_SECTION: &str = "paddock";

/// Runs the dog on a runtime of its own
pub(crate) fn main() -> ExitCode {
    let paths = match resolve_paths(&|name| std::env::var_os(name)) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("paddock: {err}.");
            return ExitCode::from(err.exit_code());
        }
    };
    let identity = DogIdentity::from_env(&|name| std::env::var(name).ok(), DEFAULT_SECTION);
    if identity.handshake().is_none() {
        eprintln!(
            "paddock: $SHEP_DOG_NAME is not set, so nothing adopted this process. It connects \
             without a name and reads [{DEFAULT_SECTION}] in dogs.toml."
        );
    }
    // The shepherd's futures are not `Send`, so everything runs on this one thread.
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
    runtime.block_on(run(identity, paths))
}

async fn run(identity: DogIdentity, paths: ShepPaths) -> ExitCode {
    let section = identity.section().to_owned();
    let runtime = match DogRuntime::start(identity, paths.clone()).await {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("paddock: cannot reach the shepherd: {err}");
            return ExitCode::FAILURE;
        }
    };
    let text = runtime.section().as_str().to_owned();
    // A section that does not validate is not retried: nothing changes until somebody edits
    // `dogs.toml`, and exiting shows the dog as down rather than up and serving nothing.
    let config = match Config::from_toml(&text) {
        Ok(config) => Arc::new(config),
        Err(err) => {
            eprintln!("paddock: the [{section}] section is not usable: {err}");
            return ExitCode::FAILURE;
        }
    };
    let live = Live::new(runtime.into_client());
    let backends = Backends::new(live.clone(), http_client());

    let state = saved::path_in(&paths.home);
    let saved = saved::load_or_empty(&state, &mut std::io::stderr());
    let discovered = discover::discover(&config, &backends, &saved).await;

    let listener = match TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("paddock: cannot listen on {}: {err}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    eprintln!("paddock: listening on {}", config.listen);

    let stop = Stop::on_stop_signals();
    let (handle, inbox) = engine::channel();
    let (sender, receiver) = watch::channel(Arc::clone(&config));
    let shared = Shared {
        engine: handle.clone(),
        config: receiver,
        http: http_client(),
        timeouts: Timeouts::default(),
    };
    let start = Start {
        state: Some(state),
        saved,
        discovered,
    };
    tokio::join!(
        engine::run(config, backends, start, inbox, stop.clone()),
        http::serve(listener, shared, stop.clone()),
        config_watch::watch(&live, &section, text, &handle, &sender, stop),
    );
    ExitCode::SUCCESS
}
