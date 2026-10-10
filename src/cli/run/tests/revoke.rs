//! A revoke reaching `run`: a model lease's command runs on, a bare lease's is stopped. Real
//! time, as in the parent module.

use std::{
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

use super::{
    GRANTED, args, bounded, link, quiet,
    signals::{Said, slow_dog},
};
use crate::{
    book::{Ended, Revocation},
    cli::{RunArgs, run::run},
    config::ClientName,
    http::lease::stream::ended_line,
};

// The dog's own line, so the reader and the writer cannot drift apart.
pub(super) static REVOKED: LazyLock<String> = LazyLock::new(|| {
    let revocation = Revocation {
        by: ClientName::from("mac-sessions"),
        note: Some("forgotten since Tuesday".to_owned()),
    };
    format!("{}\n", ended_line(&Ended::Revoked(revocation)))
});

pub(super) fn bare_args(command: &[&str]) -> RunArgs {
    RunArgs {
        model: None,
        vram: Some("8G".to_owned()),
        ram: Some("2G".to_owned()),
        grace: Duration::from_millis(200),
        ..args(command)
    }
}

/// A command that sets `trap` for TERM, says it is ready, then waits ten seconds at most.
pub(super) fn trapping(trap: &str, ready: &Path) -> String {
    format!(
        "trap {trap} TERM; touch {}; n=0; while [ $n -lt 200 ]; do sleep 0.05; n=$((n+1)); done; exit 7",
        ready.display()
    )
}

#[tokio::test]
async fn a_revoked_bare_lease_terms_then_kills_a_command_that_ignores_term() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let ready = dir.path().join("ready");
    let script = trapping("''", &ready);
    let revoke = Arc::new(Notify::new());
    let (url, lines) = slow_dog(GRANTED, Some((Arc::clone(&revoke), REVOKED.as_str()))).await;
    let said = Said::default();
    let mut err = said.clone();
    let began = Instant::now();
    let held = link(url);
    let command = bare_args(&["sh", "-c", &script]);
    let mut signals = quiet();
    let running = run(&held, &command, &mut err, &mut signals);
    let revoking = async {
        // The trap is set before the file appears, so the TERM cannot arrive too early.
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        revoke.notify_one();
        said.until("stopping the command").await;
        // Halfway through the grace, so the command is still running.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let seen = lines.lock().expect("seen lock").clone();
        assert!(
            !seen.iter().any(|line| line == "closed"),
            "the connection stays open while the job may run: {seen:?}"
        );
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, revoking) }).await;
    let text = said.text();
    assert_eq!(code, 137, "killed: {text}");
    assert!(
        began.elapsed() >= Duration::from_millis(200),
        "KILL waited out the grace"
    );
    assert!(
        text.contains(
            "the lease was revoked by mac-sessions: forgotten since Tuesday; stopping the command"
        ),
        "{text}"
    );
    let seen = lines.lock().expect("seen lock").clone();
    assert!(
        !seen.iter().any(|line| line.starts_with("DELETE")),
        "a revoked lease is not released: {seen:?}"
    );
    bounded("the dog seeing the hang-up", async {
        while !lines
            .lock()
            .expect("seen lock")
            .iter()
            .any(|line| line == "closed")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_revoked_bare_lease_whose_command_leaves_on_term_exits_with_its_status() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let ready = dir.path().join("ready");
    let script = trapping("'exit 3'", &ready);
    let revoke = Arc::new(Notify::new());
    let (url, _lines) = slow_dog(GRANTED, Some((Arc::clone(&revoke), REVOKED.as_str()))).await;
    let said = Said::default();
    let mut err = said.clone();
    let held = link(url);
    let command = bare_args(&["sh", "-c", &script]);
    let mut signals = quiet();
    let running = run(&held, &command, &mut err, &mut signals);
    let revoking = async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        revoke.notify_one();
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, revoking) }).await;
    assert_eq!(code, 3, "{}", said.text());
    assert!(
        !said.text().contains("did not stop within"),
        "{}",
        said.text()
    );
}

#[tokio::test]
async fn a_bare_lease_gone_when_attaching_again_stops_its_command() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let ready = dir.path().join("ready");
    let script = trapping("'exit 3'", &ready);
    // This dog goes silent after the grant and answers every attach with a 404.
    let (url, lines) = slow_dog(GRANTED, None).await;
    let said = Said::default();
    let mut err = said.clone();
    let mut held = link(url);
    // Long enough for the shell to set its trap before the stream counts as broken.
    held.silence = Duration::from_millis(500);
    let command = bare_args(&["sh", "-c", &script]);
    let code = bounded("the run", run(&held, &command, &mut err, &mut quiet())).await;
    let text = said.text();
    assert!(ready.exists(), "the trap was set: {text}");
    assert_eq!(code, 3, "the command got TERM: {text}");
    assert!(
        text.contains("the lease is gone; stopping the command"),
        "{text}"
    );
    let seen = lines.lock().expect("seen lock").clone();
    assert!(seen.iter().any(|line| line.contains("/attach")), "{seen:?}");
    assert!(
        !seen.iter().any(|line| line.starts_with("DELETE")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_revoked_model_leases_command_runs_on() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let go = dir.path().join("go");
    let script = format!(
        "n=0; while [ ! -e {} ] && [ $n -lt 200 ]; do sleep 0.05; n=$((n+1)); done; exit 4",
        go.display()
    );
    let revoke = Arc::new(Notify::new());
    let (url, lines) = slow_dog(GRANTED, Some((Arc::clone(&revoke), REVOKED.as_str()))).await;
    let said = Said::default();
    let mut err = said.clone();
    let held = link(url);
    let command = args(&["sh", "-c", &script]);
    let mut signals = quiet();
    let running = run(&held, &command, &mut err, &mut signals);
    let revoking = async {
        revoke.notify_one();
        said.until("letting the command finish").await;
        std::fs::write(&go, "").expect("go");
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, revoking) }).await;
    let text = said.text();
    assert_eq!(code, 4, "the command ran on: {text}");
    assert!(
        text.contains("the lease was revoked by mac-sessions: forgotten since Tuesday; letting the command finish"),
        "{text}"
    );
    let seen = lines.lock().expect("seen lock").clone();
    assert!(
        !seen.iter().any(|line| line.starts_with("DELETE")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_bare_take_carries_its_footprint_and_runs_own_pid() {
    let (url, server) = crate::test_support::fake_http(vec![
        ("POST", "/paddock/leases", vec![(200, GRANTED)]),
        ("DELETE", "/paddock/leases/L1", vec![super::RELEASED]),
    ]);
    let mut err = Vec::new();
    let code = bounded(
        "the run",
        run(&link(url), &bare_args(&["true"]), &mut err, &mut quiet()),
    )
    .await;
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let body: serde_json::Value =
        serde_json::from_str(&super::took(&server).body).expect("a JSON body");
    assert_eq!(
        body["footprint"],
        serde_json::json!({ "vram": "8G", "ram": "2G" })
    );
    assert_eq!(body["pid"], serde_json::json!(std::process::id()));
    assert!(body.get("model").is_none(), "{body}");
}
