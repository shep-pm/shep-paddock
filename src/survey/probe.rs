//! What the survey reads off the host itself: `nvidia-smi`, and a GPU process's arguments.

use core::{fmt, time::Duration};

use futures_util::{FutureExt as _, future::LocalBoxFuture};

// nvidia-smi answers in well under a second; one that hangs is a wedged driver.
const SMI_TIMEOUT: Duration = Duration::from_secs(5);
// procfs answers in microseconds. A read past this waits on a process's memory lock, which a
// process stuck in the GPU driver can hold for good.
const PROC_TIMEOUT: Duration = Duration::from_secs(1);

/// What `nvidia-smi` printed for the two queries [`super::gpu::reading`] reads
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GpuText {
    /// The memory query's output.
    pub totals: String,
    /// The compute apps query's output.
    pub apps: String,
}

/// What the survey reads off the host itself
///
/// Boxed futures so it can sit behind `dyn`.
pub(crate) trait HostProbe: fmt::Debug {
    /// What nvidia-smi prints for the totals and the compute apps, or `None` without it
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>>;

    /// The arguments `pid` runs with, or `None` when it is gone or cannot be read
    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Option<Vec<String>>>;
}

/// The host as it is: `nvidia-smi` on the `PATH`, and `/proc` for a process's arguments
///
/// `/proc` is Linux's, so elsewhere no arguments are found and an ollama model's VRAM is
/// unmeasured.
#[derive(Debug)]
pub(crate) struct NvidiaSmi;

impl HostProbe for NvidiaSmi {
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>> {
        async {
            let totals = smi(&[
                "--query-gpu=memory.used,memory.total",
                "--format=csv,noheader",
            ])
            .await?;
            let apps = smi(&[
                "--query-compute-apps=pid,process_name,used_memory",
                "--format=csv,noheader",
            ])
            .await?;
            Some(GpuText { totals, apps })
        }
        .boxed_local()
    }

    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Option<Vec<String>>> {
        blocking_within(PROC_TIMEOUT, move || {
            let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            Some(arguments(&bytes))
        })
        .boxed_local()
    }
}

/// What `read` returns, run off the engine's thread, or `None` when it takes longer than `limit`
///
/// A read stuck in the kernel holds one blocking-pool thread, never the engine.
async fn blocking_within<T: Send + 'static>(
    limit: Duration,
    read: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    tokio::time::timeout(limit, tokio::task::spawn_blocking(read))
        .await
        .ok()?
        .ok()?
}

/// `/proc/<pid>/cmdline`'s NUL-separated arguments, as text
fn arguments(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

/// What `nvidia-smi` prints with `args`, or `None` when it is missing, fails or hangs
async fn smi(args: &[&str]) -> Option<String> {
    let run = tokio::process::Command::new("nvidia-smi")
        .args(args)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(SMI_TIMEOUT, run).await.ok()?.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::{arguments, blocking_within};

    // Real time: the read runs on a blocking-pool thread, which a paused clock does not wait for.
    #[tokio::test]
    async fn a_read_that_hangs_is_given_up_on() {
        let (release, held) = std::sync::mpsc::channel::<()>();
        let read = blocking_within(Duration::from_millis(50), move || held.recv().ok());

        let got = tokio::time::timeout(Duration::from_secs(5), read).await;

        assert_eq!(got, Ok(None), "given up on, not waited for");
        release.send(()).expect("the stuck read still waits");
    }

    // Real time, as above.
    #[tokio::test]
    async fn a_read_that_answers_is_returned() {
        let read = blocking_within(Duration::from_secs(5), || Some(7));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read).await,
            Ok(Some(7))
        );
    }

    #[test]
    fn cmdline_bytes_split_at_each_nul() {
        assert_eq!(
            arguments(b"/usr/bin/llama-server\0--model\0/m/blobs/sha256-ab\0"),
            ["/usr/bin/llama-server", "--model", "/m/blobs/sha256-ab"]
        );
        assert!(arguments(b"").is_empty());
    }
}
