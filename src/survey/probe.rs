//! What the survey reads off the host itself: `nvidia-smi`, and a GPU process's arguments.

use core::{fmt, time::Duration};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

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

/// What reading a process's arguments found
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Args {
    /// Its arguments.
    Read(Vec<String>),
    /// No such process.
    Gone,
    /// Not read: the read failed, timed out, or an earlier one is still stuck. It may run anything.
    Unknown,
}

/// What the survey reads off the host itself
///
/// Boxed futures so it can sit behind `dyn`.
pub(crate) trait HostProbe: fmt::Debug {
    /// What nvidia-smi prints for the totals and the compute apps, or `None` without it
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>>;

    /// The arguments `pid` runs with
    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Args>;
}

/// The host as it is: `nvidia-smi` on the `PATH`, and `/proc` for a process's arguments
///
/// `/proc` is Linux's, so elsewhere no arguments are found and an ollama model's VRAM is
/// unmeasured.
#[derive(Debug, Default)]
pub(crate) struct NvidiaSmi {
    reads: InFlight,
}

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

    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Args> {
        let read = self.reads.read(pid, PROC_TIMEOUT, move || {
            Some(match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(bytes) => Args::Read(arguments(&bytes)),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Args::Gone,
                Err(_) => Args::Unknown,
            })
        });
        async { read.await.unwrap_or(Args::Unknown) }.boxed_local()
    }
}

/// The pids with a blocking read under way
///
/// A read stuck in the kernel stays stuck after its timeout, so its pid is skipped until it
/// returns. One wedged process then holds one blocking-pool thread, however many surveys run.
#[derive(Debug, Default, Clone)]
struct InFlight(Arc<Mutex<BTreeSet<u32>>>);

impl InFlight {
    /// What `read` returns, run off the engine's thread, or `None` when it takes longer than
    /// `limit` or a read for `pid` is still under way
    async fn read<T: Send + 'static>(
        &self,
        pid: u32,
        limit: Duration,
        read: impl FnOnce() -> Option<T> + Send + 'static,
    ) -> Option<T> {
        if !self.pids().insert(pid) {
            return None;
        }
        let reads = self.clone();
        let task = tokio::task::spawn_blocking(move || {
            let got = read();
            reads.pids().remove(&pid);
            got
        });
        tokio::time::timeout(limit, task).await.ok()?.ok()?
    }

    fn pids(&self) -> MutexGuard<'_, BTreeSet<u32>> {
        // The set is whole after any panic: each update is one insert or remove.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::{InFlight, arguments};

    const PID: u32 = 190_784;

    // Real time: the read runs on a blocking-pool thread, which a paused clock does not wait for.
    #[tokio::test]
    async fn a_read_that_hangs_is_given_up_on() {
        let reads = InFlight::default();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let read = reads.read(PID, Duration::from_millis(50), move || held.recv().ok());

        let got = tokio::time::timeout(Duration::from_secs(5), read).await;

        assert_eq!(got, Ok(None), "given up on, not waited for");
        release.send(()).expect("the stuck read still waits");
    }

    // Real time, as above.
    #[tokio::test]
    async fn a_pid_whose_read_is_stuck_is_not_read_again_until_it_returns() {
        let reads = InFlight::default();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let stuck = reads.read(PID, Duration::from_millis(50), move || held.recv().ok());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), stuck).await,
            Ok(None)
        );

        let ran = Arc::new(AtomicBool::new(false));
        let again = {
            let ran = Arc::clone(&ran);
            reads.read(PID, Duration::from_secs(5), move || {
                ran.store(true, Ordering::SeqCst);
                Some(())
            })
        };
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), again).await,
            Ok(None)
        );
        assert!(
            !ran.load(Ordering::SeqCst),
            "no second thread for a stuck pid"
        );
        let other = reads.read(PID + 1, Duration::from_secs(5), || Some(1));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), other).await,
            Ok(Some(1))
        );

        release.send(()).expect("the stuck read still waits");
        let freed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if reads.read(PID, Duration::from_secs(5), || Some(2)).await == Some(2) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(freed.is_ok(), "read again once the stuck read returned");
    }

    // Real time, as above.
    #[tokio::test]
    async fn a_read_that_answers_is_returned() {
        let reads = InFlight::default();
        let read = reads.read(PID, Duration::from_secs(5), || Some(7));
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
