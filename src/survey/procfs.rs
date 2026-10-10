//! What `/proc` and `/sys/fs/cgroup` say of a process: its parent, its resident memory, and
//! the processes in its cgroup.
//!
//! The parsers are pure. The reads block, so the probe runs them off the engine's thread.

use std::{collections::BTreeSet, path::Path};

// cgroup v2 has one hierarchy, mounted here.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
// A podman container's processes sit one cgroup below its scope; eight levels bounds a deep tree.
const CGROUP_DEPTH: usize = 8;
const KIB: u64 = 1 << 10;

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

/// Every pid in the cgroup at `dir` and the cgroups below it, as far as they can be read
pub(crate) fn cgroup_tree(dir: &Path) -> BTreeSet<u32> {
    let mut pids = BTreeSet::new();
    let mut dirs = vec![(dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        if let Ok(text) = std::fs::read_to_string(dir.join("cgroup.procs")) {
            pids.extend(procs(&text));
        }
        if depth == CGROUP_DEPTH {
            continue;
        }
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push((entry.path(), depth + 1));
            }
        }
    }
    pids
}

/// The pids in `pid`'s cgroup and below it, or `None` when its cgroup cannot be read
pub(crate) fn cgroup_pids(pid: u32) -> Option<BTreeSet<u32>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cgroup_path(&text)?;
    Some(cgroup_tree(
        &Path::new(CGROUP_ROOT).join(path.trim_start_matches('/')),
    ))
}

/// `pid`'s resident memory in bytes, or `None` when it cannot be read
pub(crate) fn rss(pid: u32) -> Option<u64> {
    vm_rss(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// `pid`'s parent, or `None` when it cannot be read
pub(crate) fn parent(pid: u32) -> Option<u32> {
    ppid(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{cgroup_path, cgroup_tree, ppid, procs, vm_rss};

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
            BTreeSet::from([1_246_083, 1_246_137, 1_246_200])
        );
    }
}
