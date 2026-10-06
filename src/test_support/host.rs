//! A scripted host for the survey.

use std::collections::BTreeMap;

use futures_util::{FutureExt as _, future::LocalBoxFuture};

use crate::survey::probe::{GpuText, HostProbe};

/// A host whose `nvidia-smi` prints what the test says, or is missing, so a survey runs without a GPU.
///
/// A pid's arguments are found only when [`Self::with_cmdline`] gave them.
#[derive(Debug, Default)]
pub(crate) struct FakeHost {
    gpu: Option<GpuText>,
    cmdlines: BTreeMap<u32, Vec<String>>,
}

impl FakeHost {
    /// A host without `nvidia-smi`.
    pub(crate) fn absent() -> Self {
        Self::default()
    }

    /// A host whose `nvidia-smi` prints `totals` for the memory query and `apps` for the compute apps.
    pub(crate) fn printing(totals: &str, apps: &str) -> Self {
        Self {
            gpu: Some(GpuText {
                totals: totals.to_owned(),
                apps: apps.to_owned(),
            }),
            cmdlines: BTreeMap::new(),
        }
    }

    /// The same host, where `pid` runs with `args`.
    pub(crate) fn with_cmdline(mut self, pid: u32, args: Vec<String>) -> Self {
        self.cmdlines.insert(pid, args);
        self
    }
}

impl HostProbe for FakeHost {
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>> {
        core::future::ready(self.gpu.clone()).boxed_local()
    }

    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Option<Vec<String>>> {
        core::future::ready(self.cmdlines.get(&pid).cloned()).boxed_local()
    }
}
