//! What `/proc` and `/sys/fs/cgroup` say of a process: its parent, its resident memory, and
//! the processes in its cgroup.
//!
//! The parsers are pure. The reads block, so the probe runs them off the engine's thread.

use std::{collections::BTreeSet, path::Path};

use super::probe::Resident;

// cgroup v2 has one hierarchy, mounted here.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
// A podman container's processes sit one cgroup below its scope; eight levels bounds a deep tree.
const CGROUP_DEPTH: usize = 8;
const KIB: u64 = 1 << 10;
// Linux's errno for a process that no longer exists.
const ESRCH: i32 = 3;

/// The cgroup v2 path in `/proc/<pid>/cgroup`'s text: its `0::` line's
///
/// Only a path of plain names below the root is taken, so a read never leaves the cgroup tree.
/// The root itself is refused: its tree is every process on the host.
pub(crate) fn cgroup_path(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .filter(|path| {
            path.strip_prefix('/').is_some_and(|below| {
                below
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | ".."))
            })
        })
}

/// The pids a `cgroup.procs` file lists
pub(crate) fn procs(text: &str) -> BTreeSet<u32> {
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// The resident memory `/proc/<pid>/status` gives, in bytes
pub(crate) fn vm_rss(text: &str) -> Option<u64> {
    let line = text.lines().find_map(|line| line.strip_prefix("VmRSS:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(KIB)
}

/// The parent pid `/proc/<pid>/stat` gives
///
/// The command name sits in parentheses and may hold spaces or parentheses itself, so the
/// fields are read from after its last `)`.
pub(crate) fn ppid(text: &str) -> Option<u32> {
    let (_, rest) = text.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Every pid in the cgroup at `dir` and the cgroups below it, or `None` when one cannot be read
///
/// A cgroup below `dir` that is gone when read was removed during the walk, and holds nothing.
pub(crate) fn cgroup_tree(dir: &Path) -> Option<BTreeSet<u32>> {
    let mut pids = BTreeSet::new();
    let mut dirs = vec![(dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let gone = |err: &std::io::Error| depth > 0 && err.kind() == std::io::ErrorKind::NotFound;
        match std::fs::read_to_string(dir.join("cgroup.procs")) {
            Ok(text) => pids.extend(procs(&text)),
            Err(err) if gone(&err) => continue,
            Err(_) => return None,
        }
        if depth == CGROUP_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if gone(&err) => continue,
            Err(_) => return None,
        };
        for entry in entries {
            let entry = entry.ok()?;
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push((entry.path(), depth + 1));
            }
        }
    }
    Some(pids)
}

/// The pids in `pid`'s cgroup and below it, or `None` when a cgroup in it cannot be read
pub(crate) fn cgroup_pids(pid: u32) -> Option<BTreeSet<u32>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cgroup_path(&text)?;
    cgroup_tree(&Path::new(CGROUP_ROOT).join(path.trim_start_matches('/')))
}

/// `pid`'s resident memory, or [`Resident::Gone`] when it has exited
pub(crate) fn rss(pid: u32) -> Resident {
    resident(std::fs::read_to_string(format!("/proc/{pid}/status")))
}

/// What reading a `/proc/<pid>/status` file as `read` says of its process's memory
///
/// A status without `VmRSS` is a zombie or a kernel thread, which holds none.
fn resident(read: std::io::Result<String>) -> Resident {
    match read {
        Ok(text) => vm_rss(&text).map_or(Resident::Gone, Resident::Bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Resident::Gone,
        // A process that exits between the open and the read fails the read with ESRCH.
        Err(err) if err.raw_os_error() == Some(ESRCH) => Resident::Gone,
        Err(_) => Resident::Unknown,
    }
}

/// `pid`'s parent, or `None` when it cannot be read
pub(crate) fn parent(pid: u32) -> Option<u32> {
    ppid(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, os::unix::fs::PermissionsExt as _};

    use super::{Resident, cgroup_path, cgroup_tree, ppid, procs, resident, vm_rss};

    // Built from the GPU host's cgroup line, not captured: the scope's full id is made up
    // around the short id podman printed.
    const CGROUP: &str = "0::/user.slice/user-1000.slice/user@1000.service/user.slice/libpod-55417753430f2b6d0c1e8a9f3b7c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e.scope/container\n";

    #[test]
    fn the_cgroup_path_is_the_v2_line() {
        assert_eq!(
            cgroup_path(CGROUP),
            Some(
                "/user.slice/user-1000.slice/user@1000.service/user.slice/libpod-55417753430f2b6d0c1e8a9f3b7c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e.scope/container"
            )
        );
        assert_eq!(cgroup_path("12:memory:/x\n0::/y\n"), Some("/y"));
        assert_eq!(
            cgroup_path("12:memory:/x\n"),
            None,
            "cgroup v1 alone has no path to read"
        );
    }

    #[test]
    fn a_cgroup_path_that_could_leave_the_cgroup_tree_is_refused() {
        assert_eq!(cgroup_path("0::/../x\n"), None);
        assert_eq!(cgroup_path("0::/../../..\n"), None);
        assert_eq!(cgroup_path("0::/user.slice/../../etc\n"), None);
        assert_eq!(cgroup_path("0::/user.slice/./x\n"), None);
        assert_eq!(cgroup_path("0::/user.slice//x\n"), None);
        assert_eq!(cgroup_path("0::/\n"), None, "the root holds every process");
        assert_eq!(cgroup_path("0::x\n"), None);
    }

    #[test]
    fn procs_skips_what_is_not_a_pid() {
        assert_eq!(
            procs("1246083\n1246137\n\nzombie\n"),
            BTreeSet::from([1_246_083, 1_246_137])
        );
    }

    #[test]
    fn resident_memory_is_vmrss_in_bytes() {
        assert_eq!(
            vm_rss("Name:\tstrata\nVmRSS:\t   55574528 kB\nVmSwap:\t0 kB\n"),
            Some(55_574_528 << 10)
        );
        assert_eq!(
            vm_rss("Name:\tkthreadd\n"),
            None,
            "a kernel thread has none"
        );
    }

    #[test]
    fn the_parent_is_read_past_a_command_name_with_spaces_and_parentheses() {
        assert_eq!(
            ppid("1246137 (strata engine) S 1246083 1246137 1 0"),
            Some(1_246_083)
        );
        assert_eq!(ppid("7 (a) b) R 3 7"), Some(3));
        assert_eq!(ppid("garbled"), None);
    }

    #[test]
    fn a_cgroup_tree_lists_its_own_pids_and_those_below_it() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let container = root
            .path()
            .join("libpod-55417753430f.scope")
            .join("container");
        std::fs::create_dir_all(container.join("inner")).expect("dirs");
        std::fs::write(container.join("cgroup.procs"), "1246083\n1246137\n").expect("written");
        std::fs::write(container.join("inner").join("cgroup.procs"), "1246200\n").expect("written");
        assert_eq!(
            cgroup_tree(&container),
            Some(BTreeSet::from([1_246_083, 1_246_137, 1_246_200]))
        );
    }

    #[test]
    fn a_cgroup_tree_with_a_cgroup_that_cannot_be_read_is_unknown() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let container = root.path().join("container");
        std::fs::create_dir_all(container.join("inner")).expect("dirs");
        assert_eq!(cgroup_tree(&container), None, "its own procs are missing");

        std::fs::write(container.join("cgroup.procs"), "1246083\n").expect("written");
        let inner = container.join("inner").join("cgroup.procs");
        std::fs::write(&inner, "1246200\n").expect("written");
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o000))
            .expect("made unreadable");
        assert_eq!(
            cgroup_tree(&container),
            None,
            "a cgroup below is unreadable"
        );
    }

    #[test]
    fn a_cgroup_removed_during_the_walk_holds_nothing() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let container = root.path().join("container");
        std::fs::create_dir_all(container.join("gone")).expect("dirs");
        std::fs::write(container.join("cgroup.procs"), "1246083\n").expect("written");
        assert_eq!(cgroup_tree(&container), Some(BTreeSet::from([1_246_083])));
    }

    #[test]
    fn a_status_read_failing_as_the_process_exits_holds_nothing() {
        let exited = std::io::Error::from_raw_os_error(super::ESRCH);
        assert_eq!(resident(Err(exited)), Resident::Gone);
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(resident(Err(missing)), Resident::Gone);
        let refused = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(resident(Err(refused)), Resident::Unknown);
    }

    #[test]
    fn a_zombies_status_without_vmrss_holds_nothing() {
        let zombie = "Name:\tstrata\nState:\tZ (zombie)\nTgid:\t1246137\n";
        assert_eq!(resident(Ok(zombie.to_owned())), Resident::Gone);
        let live = "Name:\tstrata\nVmRSS:\t   2048 kB\n";
        assert_eq!(resident(Ok(live.to_owned())), Resident::Bytes(2 << 20));
    }
}
