//! What the survey reads off the host itself: `nvidia-smi`, and a GPU process's arguments.

use core::{fmt, future::Future, time::Duration};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    process::Stdio,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use tokio::io::AsyncReadExt as _;

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
///
/// `Debug` counts the arguments and prints none: one can carry a key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Args {
    /// Its arguments.
    Read(Vec<String>),
    /// No such process.
    Gone,
    /// Not read: the read failed, timed out, or an earlier one is still stuck. It may run anything.
    Unknown,
}

impl fmt::Debug for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(args) => write!(f, "Read({} arguments)", args.len()),
            Self::Gone => f.write_str("Gone"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
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
    smis: OneSmi,
}

impl NvidiaSmi {
    /// What `nvidia-smi` prints with `args`, or `None` when it is missing, fails, hangs, or an
    /// earlier one is still running
    async fn query(&self, args: &'static [&'static str]) -> Option<String> {
        let run = smi("nvidia-smi".as_ref(), args, SMI_TIMEOUT);
        self.smis.run(SMI_TIMEOUT, run).await
    }
}

impl HostProbe for NvidiaSmi {
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>> {
        async {
            let totals = self
                .query(&[
                    "--query-gpu=memory.used,memory.total",
                    "--format=csv,noheader",
                ])
                .await?;
            let apps = self
                .query(&[
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
        let done = Done {
            reads: self.clone(),
            pid,
        };
        let task = tokio::task::spawn_blocking(move || {
            let _done = done;
            read()
        });
        tokio::time::timeout(limit, task).await.ok()?.ok()?
    }

    fn pids(&self) -> MutexGuard<'_, BTreeSet<u32>> {
        // The set is whole after any panic: each update is one insert or remove.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Takes its pid out of the set when dropped, so a read that panics still frees it
#[derive(Debug)]
struct Done {
    reads: InFlight,
    pid: u32,
}

impl Drop for Done {
    fn drop(&mut self) {
        self.reads.pids().remove(&self.pid);
    }
}

/// Whether an `nvidia-smi` is still running, so no second one starts beside it
///
/// A wedged driver leaves `nvidia-smi` where SIGKILL does not land. One stuck
/// process then stays one, however many surveys run.
#[derive(Debug, Default, Clone)]
struct OneSmi {
    running: Arc<AtomicBool>,
    skipping: Arc<AtomicBool>,
}

impl OneSmi {
    /// What `run` returns, as a task of its own, or `None` when it takes longer than `limit`
    /// or an earlier run has not ended
    ///
    /// The first run skipped, and the first after skips end, are logged.
    async fn run<T: Send + 'static>(
        &self,
        limit: Duration,
        run: impl Future<Output = Option<T>> + Send + 'static,
    ) -> Option<T> {
        if self.running.swap(true, Ordering::SeqCst) {
            if !self.skipping.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "paddock: an earlier nvidia-smi is still running, so the GPU goes unread"
                );
            }
            return None;
        }
        if self.skipping.swap(false, Ordering::SeqCst) {
            eprintln!("paddock: the stuck nvidia-smi has ended, so the GPU is read again");
        }
        let running = Cleared(Arc::clone(&self.running));
        let task = tokio::spawn(async move {
            let _running = running;
            run.await
        });
        tokio::time::timeout(limit, task).await.ok()?.ok()?
    }
}

/// Clears the flag it holds when dropped, so a query that panics still clears it
#[derive(Debug)]
struct Cleared(Arc<AtomicBool>);

impl Drop for Cleared {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
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

/// What `program` prints with `args`, or `None` when it is missing, fails or runs past `limit`
///
/// One that runs past `limit` is killed, and this returns only once it is reaped.
async fn smi(program: &OsStr, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut printed = Vec::new();
    let ran = tokio::time::timeout(limit, async {
        let (read, status) = tokio::join!(stdout.read_to_end(&mut printed), child.wait());
        read.ok().and(status.ok())
    })
    .await;
    let Ok(status) = ran else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return None;
    };
    status?
        .success()
        .then(|| String::from_utf8_lossy(&printed).into_owned())
}

#[cfg(test)]
mod tests;
