//! What podman says of a container: running under which pid, not running, or nothing readable.

use core::{future::Future, time::Duration};

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
    ask_by(name, |args| async move {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        exec("podman".as_ref(), &args, PODMAN_TIMEOUT).await
    })
    .await
}

/// What podman exited with and printed, `None` when it could not be run or did not answer
type Ran = Option<(Option<i32>, String)>;

/// [`ask`], with each podman command run by `run`
async fn ask_by<F, Fut>(name: &str, mut run: F) -> Container
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Ran>,
{
    let exists = || vec!["container".to_owned(), "exists".to_owned(), name.to_owned()];
    if let Some(answer) = absent(run(exists()).await) {
        return answer;
    }
    let inspect = [
        "inspect",
        "--type",
        "container",
        "--format",
        "{{.State.Pid}}",
        name,
    ];
    match run(inspect.map(str::to_owned).to_vec()).await {
        Some((Some(0), printed)) => main_pid(&printed),
        // `--rm` can remove a container between the two questions.
        _ => absent(run(exists()).await)
            .unwrap_or_else(|| Container::Unreadable("podman inspect failed".to_owned())),
    }
}

/// What `podman container exists` ending as `ran` says, or `None` when the container is there
fn absent(ran: Ran) -> Option<Container> {
    match ran {
        Some((Some(0), _)) => None,
        Some((Some(1), _)) => Some(Container::Stopped),
        Some((code, _)) => {
            let code = code.map_or_else(|| "a signal".to_owned(), |code| code.to_string());
            Some(Container::Unreadable(format!(
                "podman container exists ended with {code}"
            )))
        }
        None => Some(Container::Unreadable(
            "podman could not be run, or did not answer".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque};

    use futures_util::FutureExt as _;

    use super::{Container, Ran, ask_by, main_pid};

    const NAME: &str = "strata-qwen-iq3_xxs";

    /// What [`ask_by`] makes of podman answering `answers` in turn, and the commands it ran
    fn asked(answers: Vec<Ran>) -> (Container, Vec<String>) {
        let answers = RefCell::new(VecDeque::from(answers));
        let ran = RefCell::new(Vec::new());
        let found = ask_by(NAME, |args| {
            ran.borrow_mut().push(args[..2].join(" "));
            core::future::ready(answers.borrow_mut().pop_front().flatten())
        })
        .now_or_never()
        .expect("every answer is ready");
        (found, ran.into_inner())
    }

    fn exit(code: i32, printed: &str) -> Ran {
        Some((Some(code), printed.to_owned()))
    }

    #[test]
    fn a_running_container_is_asked_twice() {
        let (found, ran) = asked(vec![exit(0, ""), exit(0, "1246083\n")]);
        assert_eq!(found, Container::Running(1_246_083));
        assert_eq!(ran, ["container exists", "inspect --type"]);
    }

    #[test]
    fn a_container_removed_before_its_inspect_is_stopped() {
        let (found, ran) = asked(vec![exit(0, ""), exit(125, ""), exit(1, "")]);
        assert_eq!(found, Container::Stopped);
        assert_eq!(
            ran,
            ["container exists", "inspect --type", "container exists"]
        );
    }

    #[test]
    fn an_inspect_failing_on_a_container_still_there_is_unreadable() {
        let (found, _) = asked(vec![exit(0, ""), exit(125, ""), exit(0, "")]);
        assert_eq!(
            found,
            Container::Unreadable("podman inspect failed".to_owned())
        );
    }

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
