//! A scripted host for the survey.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use tokio::sync::Semaphore;

use crate::survey::{
    podman::Container,
    probe::{Args, GpuText, HostProbe, Resident},
};

/// A host whose `nvidia-smi` prints what the test says, or is missing, so a survey runs without a GPU.
///
/// A pid's arguments are found only when [`Self::with_cmdline`] gave them; any other pid is gone. A host made
/// [`Self::gated`] holds each `nvidia-smi` run until the test lets it finish. podman says a container is
/// not running unless [`Self::with_container`] said otherwise, and a pid has no cgroup, readable memory or
/// parent unless the test gave one.
#[derive(Debug, Default)]
pub(crate) struct FakeHost {
    gpu: Option<GpuText>,
    cmdlines: BTreeMap<u32, Args>,
    gate: Option<HostGate>,
    containers: BTreeMap<String, Container>,
    cgroups: BTreeMap<u32, BTreeSet<u32>>,
    rss: BTreeMap<u32, Resident>,
    parents: BTreeMap<u32, u32>,
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
            ..Self::default()
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

    /// The same host, where podman says the container `name` is `state`.
    pub(crate) fn with_container(mut self, name: &str, state: Container) -> Self {
        self.containers.insert(name.to_owned(), state);
        self
    }

    /// The same host, where `pid`'s cgroup holds `pids`.
    pub(crate) fn with_cgroup(mut self, pid: u32, pids: &[u32]) -> Self {
        self.cgroups.insert(pid, pids.iter().copied().collect());
        self
    }

    /// The same host, where `pid` holds `bytes` resident.
    pub(crate) fn with_rss(mut self, pid: u32, bytes: u64) -> Self {
        self.rss.insert(pid, Resident::Bytes(bytes));
        self
    }

    /// The same host, where `pid` exited before its memory was read.
    pub(crate) fn with_rss_gone(mut self, pid: u32) -> Self {
        self.rss.insert(pid, Resident::Gone);
        self
    }

    /// The same host, where `pid`'s parent is `parent`.
    pub(crate) fn with_parent(mut self, pid: u32, parent: u32) -> Self {
        self.parents.insert(pid, parent);
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

    fn container(&self, name: &str) -> LocalBoxFuture<'_, Container> {
        let state = self
            .containers
            .get(name)
            .cloned()
            .unwrap_or(Container::Stopped);
        core::future::ready(state).boxed_local()
    }

    fn cgroup_pids(&self, pid: u32) -> LocalBoxFuture<'_, Option<BTreeSet<u32>>> {
        core::future::ready(self.cgroups.get(&pid).cloned()).boxed_local()
    }

    fn rss(&self, pid: u32) -> LocalBoxFuture<'_, Resident> {
        let resident = self.rss.get(&pid).copied().unwrap_or(Resident::Unknown);
        core::future::ready(resident).boxed_local()
    }

    fn parent(&self, pid: u32) -> LocalBoxFuture<'_, Option<u32>> {
        core::future::ready(self.parents.get(&pid).copied()).boxed_local()
    }
}
