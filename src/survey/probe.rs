//! What the survey reads off the host itself: `nvidia-smi`, and a GPU process's arguments.

use core::{fmt, time::Duration};

use futures_util::{FutureExt as _, future::LocalBoxFuture};

// nvidia-smi answers in well under a second; one that hangs is a wedged driver.
const SMI_TIMEOUT: Duration = Duration::from_secs(5);

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
        // procfs answers from the kernel's memory, so this read does not block the thread.
        let args = std::fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .map(|bytes| arguments(&bytes));
        core::future::ready(args).boxed_local()
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
    use super::arguments;

    #[test]
    fn cmdline_bytes_split_at_each_nul() {
        assert_eq!(
            arguments(b"/usr/bin/llama-server\0--model\0/m/blobs/sha256-ab\0"),
            ["/usr/bin/llama-server", "--model", "/m/blobs/sha256-ab"]
        );
        assert!(arguments(b"").is_empty());
    }
}
