//! What podman says of a container: running under which pid, not running, or nothing readable.

use core::time::Duration;

use super::probe::exec;

// podman answers a local question in well under a second; one past this is wedged.
const PODMAN_TIMEOUT: Duration = Duration::from_secs(10);

/// A podman container, as one survey found it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Container {
    /// Running, with this main pid.
    Running(u32),
    /// Not running, or gone: the Strata script's `--rm` removes a stopped one.
    Stopped,
    /// podman could not be asked, or answered what the dog cannot read.
    Unreadable(String),
}

/// What `podman inspect --format {{.State.Pid}}` printing `printed` says, where `0` is not running
pub(crate) fn main_pid(printed: &str) -> Container {
    match printed.trim().parse::<u32>() {
        Ok(0) => Container::Stopped,
        Ok(pid) => Container::Running(pid),
        Err(_) => Container::Unreadable(format!("podman inspect printed {:?}", printed.trim())),
    }
}

/// What podman, run as the dog's own user, says of the container `name`
///
/// `podman container exists` exiting 1 is a container that is not there. Any other failure,
/// and a podman that is missing or hangs, is unreadable.
pub(crate) async fn ask(name: &str) -> Container {
    match exec(
        "podman".as_ref(),
        &["container", "exists", name],
        PODMAN_TIMEOUT,
    )
    .await
    {
        Some((Some(0), _)) => {}
        Some((Some(1), _)) => return Container::Stopped,
        Some((code, _)) => {
            let code = code.map_or_else(|| "a signal".to_owned(), |code| code.to_string());
            return Container::Unreadable(format!("podman container exists ended with {code}"));
        }
        None => {
            return Container::Unreadable("podman could not be run, or did not answer".to_owned());
        }
    }
    let format = [
        "inspect",
        "--type",
        "container",
        "--format",
        "{{.State.Pid}}",
        name,
    ];
    match exec("podman".as_ref(), &format, PODMAN_TIMEOUT).await {
        Some((Some(0), printed)) => main_pid(&printed),
        _ => Container::Unreadable("podman inspect failed".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Container, main_pid};

    #[test]
    fn a_main_pid_is_running_and_zero_is_not() {
        assert_eq!(main_pid("1246083\n"), Container::Running(1_246_083));
        assert_eq!(main_pid("0\n"), Container::Stopped);
    }

    #[test]
    fn an_unreadable_main_pid_is_podman_failing() {
        assert_eq!(
            main_pid("<no value>\n"),
            Container::Unreadable("podman inspect printed \"<no value>\"".to_owned())
        );
    }
}
