//! A scripted host for the survey.

use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use tokio::sync::Semaphore;

use crate::survey::probe::{Args, GpuText, HostProbe};

/// A host whose `nvidia-smi` prints what the test says, or is missing, so a survey runs without a GPU.
///
/// A pid's arguments are found only when [`Self::with_cmdline`] gave them; any other pid is gone. A host made
/// [`Self::gated`] holds each `nvidia-smi` run until the test lets it finish.
#[derive(Debug, Default)]
pub(crate) struct FakeHost {
    gpu: Option<GpuText>,
    cmdlines: BTreeMap<u32, Args>,
    gate: Option<HostGate>,
}

/// The test's side of a gated [`FakeHost`]
#[derive(Debug, Clone)]
pub(crate) struct HostGate {
    permits: Rc<Semaphore>,
    asked: Rc<Cell<usize>>,
}

impl HostGate {
    /// Lets one waiting or later `nvidia-smi` run finish.
    pub(crate) fn open(&self) {
        self.permits.add_permits(1);
    }

    /// How many `nvidia-smi` runs began.
    pub(crate) fn asked(&self) -> usize {
        self.asked.get()
    }
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
            gate: None,
        }
    }

    /// The same host, with its `nvidia-smi` runs held at the returned gate.
    pub(crate) fn gated(mut self) -> (Self, HostGate) {
        let gate = HostGate {
            permits: Rc::new(Semaphore::new(0)),
            asked: Rc::new(Cell::new(0)),
        };
        self.gate = Some(gate.clone());
        (self, gate)
    }

    /// The same host, where `pid` runs with `args`.
    pub(crate) fn with_cmdline(mut self, pid: u32, args: Vec<String>) -> Self {
        self.cmdlines.insert(pid, Args::Read(args));
        self
    }

    /// The same host, where `pid`'s arguments cannot be read, as when `/proc` times out.
    pub(crate) fn with_cmdline_unread(mut self, pid: u32) -> Self {
        self.cmdlines.insert(pid, Args::Unknown);
        self
    }
}

impl HostProbe for FakeHost {
    fn gpu(&self) -> LocalBoxFuture<'_, Option<GpuText>> {
        async {
            if let Some(gate) = &self.gate {
                gate.asked.set(gate.asked.get() + 1);
                if let Ok(permit) = gate.permits.acquire().await {
                    permit.forget();
                }
            }
            self.gpu.clone()
        }
        .boxed_local()
    }

    fn cmdline(&self, pid: u32) -> LocalBoxFuture<'_, Args> {
        let args = self.cmdlines.get(&pid).cloned().unwrap_or(Args::Gone);
        core::future::ready(args).boxed_local()
    }
}
