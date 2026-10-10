//! Stopping a model's podman container once its sheep has stopped.

use core::{fmt, time::Duration};
use std::process::Stdio;

use futures_util::{FutureExt as _, future::LocalBoxFuture};

// podman stop gives the container 10 s before it kills it. This leaves room, inside the 30 s
// one unload attempt gets.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

/// What stops a podman container
pub(crate) trait Containers: fmt::Debug {
    /// Stops the container `name`; one already gone counts as stopped
    ///
    /// # Errors
    /// What went wrong, in words for the dog's log.
    fn stop(&self, name: &str) -> LocalBoxFuture<'_, Result<(), String>>;
}

/// podman on the `PATH`, run as the dog's own user
#[derive(Debug, Default)]
pub(crate) struct Podman;

impl Containers for Podman {
    fn stop(&self, name: &str) -> LocalBoxFuture<'_, Result<(), String>> {
        let name = name.to_owned();
        async move {
            let stopping = tokio::process::Command::new("podman")
                .arg("stop")
                .arg("--ignore")
                .arg(&name)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status();
            match tokio::time::timeout(STOP_TIMEOUT, stopping).await {
                Ok(Ok(status)) if status.success() => Ok(()),
                Ok(Ok(status)) => Err(format!("podman stop {name} ended with {status}")),
                Ok(Err(err)) => Err(format!("podman could not be run: {err}")),
                Err(_) => Err(format!(
                    "podman stop {name} did not end within {STOP_TIMEOUT:?}"
                )),
            }
        }
        .boxed_local()
    }
}
