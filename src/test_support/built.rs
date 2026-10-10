//! Fixtures built, not captured, shared by the survey's tests and the engine's.
//!
//! The container figures are built from what the maintainer measured on the GPU host on
//! 2026-10-10: the sheep's podman client held 106 MiB and no GPU memory, and the container's main
//! pid 1246083 and its child 1246137, the engine, held 53 GiB of RAM and 23.8 GiB of GPU memory.
//! [`ENGINE_APP`]'s 23800 MiB is a round figure in `nvidia-smi`'s unit, not that measurement.

use std::collections::{BTreeMap, BTreeSet};

use crate::survey::ContainerRead;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// The sheep's podman client.
pub(crate) const CLIENT: u32 = 1_246_070;
/// The container's main pid.
pub(crate) const MAIN: u32 = 1_246_083;
/// The engine, the main pid's child, which holds the GPU memory.
pub(crate) const ENGINE: u32 = 1_246_137;
/// `nvidia-smi`'s compute apps with the engine running.
pub(crate) const ENGINE_APP: &str = "1246137, /opt/strata/engine/strata, 23800 MiB\n";

/// GPU processes: 5001 and 5002 under a bare job's pid 4321, 7000 under init.
pub(crate) const BARE_APPS: &str = "5001, /usr/bin/python3, 6000 MiB\n5002, /usr/bin/python3, 1000 MiB\n7000, /usr/bin/other, 500 MiB\n";

/// The parents of [`BARE_APPS`]' processes.
pub(crate) fn bare_parents() -> BTreeMap<u32, u32> {
    BTreeMap::from([(5001, 4321), (5002, 5001), (7000, 1)])
}

/// What the container held: its main pid and the engine.
pub(crate) fn contained() -> ContainerRead {
    ContainerRead {
        pids: BTreeSet::from([MAIN, ENGINE]),
        ram: 2 * MIB + 53 * GIB,
    }
}
