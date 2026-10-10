//! A bare lease's command runs in its own process group, and a stop reaches the whole group.
//! Real time, as in the parent module.

use std::{path::Path, sync::Arc, time::Duration};

use tokio::{process::Command, sync::Notify, time::Instant};

use super::{
    GRANTED, RELEASED, bounded, fake_http, link, quiet,
    revoke::{REVOKED, bare_args, trapping},
    signals::{Said, slow_dog},
};
use crate::cli::{Forward, RunArgs, run::run};

/// Whether the process `pid` still exists.
async fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// A command that leaves on TERM, with a child that ignores TERM and sleeps. Each writes its pid.
fn leaves_a_child(parent: &Path, child: &Path, ready: &Path) -> String {
    format!(
        "echo $$ > {}; sh -c 'trap \"\" TERM; echo $$ > {}; touch {}; exec sleep 30' & wait",
        parent.display(),
        child.display(),
        ready.display()
    )
}

async fn gone_within(limit: Duration, pid_file: &Path) -> bool {
    let pid = std::fs::read_to_string(pid_file).expect("a pid");
    let gone = async {
        while alive(pid.trim()).await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(limit, gone).await.is_ok()
}

#[tokio::test]
async fn a_revoke_kills_what_a_bare_command_left_and_holds_the_stream_until_then() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let parent = dir.path().join("parent");
    let (child, ready) = (dir.path().join("child"), dir.path().join("ready"));
    let script = leaves_a_child(&parent, &child, &ready);
    let revoke = Arc::new(Notify::new());
    let (url, lines) = slow_dog(GRANTED, Some((Arc::clone(&revoke), REVOKED.as_str()))).await;
    let said = Said::default();
    let mut err = said.clone();
    let began = Instant::now();
    let held = link(url);
    let grace = Duration::from_millis(500);
    let command = RunArgs {
        grace,
        ..bare_args(&["sh", "-c", &script])
    };
    let mut signals = quiet();
    let running = run(&held, &command, &mut err, &mut signals);
    let revoking = async {
        // The child's trap is set before the file appears, so the TERM cannot arrive too early.
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        revoke.notify_one();
        let revoked = Instant::now();
        assert!(
            gone_within(grace, &parent).await,
            "the command left on TERM"
        );
        let seen = lines.lock().expect("seen lock").clone();
        assert!(revoked.elapsed() < grace, "checked inside the grace");
        assert!(
            !seen.iter().any(|line| line == "closed"),
            "the stream is held after the command left: {seen:?}"
        );
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, revoking) }).await;
    let text = said.text();
    assert_eq!(code, 143, "the command's own status: {text}");
    assert!(began.elapsed() >= grace, "KILL waited out the grace");
    assert!(text.contains("did not stop within"), "{text}");
    assert!(
        gone_within(Duration::from_secs(2), &child).await,
        "the child that ignored TERM was killed"
    );
}

#[tokio::test]
async fn an_interrupt_sent_to_a_bare_run_reaches_its_command() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let ready = dir.path().join("ready");
    let script = trapping("'exit 0'", &ready).replace("TERM", "INT");
    let (url, _server) = fake_http(vec![
        ("POST", "/paddock/leases", vec![(200, GRANTED)]),
        ("POST", "/paddock/leases/L1/attach", vec![(200, GRANTED)]),
        ("DELETE", "/paddock/leases/L1", vec![RELEASED]),
    ]);
    let (sender, mut signals) = tokio::sync::mpsc::unbounded_channel();
    let mut err = Vec::new();
    let held = link(url);
    let command = bare_args(&["sh", "-c", &script]);
    let running = run(&held, &command, &mut err, &mut signals);
    let sending = async {
        // The trap is set before the file appears, so the signal cannot arrive too early.
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sender.send(Forward::Interrupt).expect("the run listens");
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, sending) }).await;
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
}
