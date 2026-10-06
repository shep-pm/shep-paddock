//! The tier that drives the REAL dog against a REAL shepherd.
//!
//! Every module below `main` has unit tests built on a fake shepherd, and all of them can pass
//! while the dog never connects, never reads its section, or never starts a sheep. `dog::run` has
//! no unit test at all. This file is what covers it: the dog binary is adopted by a shepherd of
//! its own, serves on a loopback port, and starts and stops stub sheep that run
//! `python3 -m http.server`. Loading a real model is never part of this tier.
//!
//! Gated behind the `integration` feature, and needing `$SHEP_BIN` pointed at a built `shep`:
//!
//! ```text
//! cargo install --git https://github.com/shep-pm/shep --branch main shep --bin shep --locked
//! SHEP_BIN="$(command -v shep)" cargo test --features integration --locked
//! ```
//!
//! # `$SHEP_HOME` is a temporary directory in every test here
//!
//! A live shepherd may run at `~/.shep`, supervising real services. Every test builds its own
//! [`Shepherd`], which owns a temporary directory and kills the daemon it booted when it drops.
//! `--home` alone is not enough: `shep adopt` vets a binary by spawning it with this process's
//! environment, so every command also sets `SHEP_HOME` in the child's environment.

// Under `integration/`, so cargo does not build either as a test target of its own.
#[path = "integration/bounded.rs"]
mod bounded;
#[path = "integration/harness.rs"]
mod harness;

use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use bounded::*;
use harness::*;

/// A drop runs its cleanup with the non-panicking form, so a cleanup that overruns cannot panic
/// a second time while a failed test unwinds, which would abort the whole run.
#[test]
fn a_command_past_its_limit_is_killed_and_reported_without_a_panic() {
    let started = Instant::now();
    let output = Command::new("sleep")
        .arg("30")
        .try_output_within(Duration::from_millis(200));
    assert!(output.is_none());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        Command::new("/nonexistent/shep-paddock-test")
            .try_output_within(PATIENCE)
            .is_none()
    );
}

#[test]
fn a_request_loads_the_sheep_and_is_forwarded() {
    let alpha = Stub::new("alpha");
    let shepherd = Shepherd::with_dog(&[&alpha]);

    // Nothing has asked for it yet, so the dog started with nothing loaded.
    assert_eq!(shepherd.state_of("alpha").as_deref(), Some("unloaded"));
    let unkeyed = http(shepherd.dog_port, "GET", "/alpha/", None);
    assert_eq!(unkeyed.status, 401, "{}", unkeyed.body);

    // The prefix route forwards any method with the prefix stripped, so this reaches the stub's
    // directory listing only if the dog started the sheep, polled it ready and proxied.
    let answer = shepherd.get("/alpha/");
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.body.contains("Directory listing"), "{}", answer.body);
    assert_eq!(shepherd.state_of("alpha").as_deref(), Some("loaded"));

    let flock = shepherd.ok(&["list", "--format", "json"]);
    assert!(flock.contains("alpha"), "{flock}");
    let models = http(shepherd.dog_port, "GET", "/v1/models", None);
    assert_eq!(models.status, 200, "{}", models.body);
    assert!(models.body.contains("alpha"), "{}", models.body);
}

/// The stub has no probe, so the shepherd says it is online as soon as it starts.
#[test]
fn a_model_without_a_ready_check_loads_once_its_sheep_is_online() {
    let alpha = Stub::without_ready("alpha");
    let shepherd = Shepherd::with_dog(&[&alpha]);
    assert_eq!(shepherd.state_of("alpha").as_deref(), Some("unloaded"));

    // Forwarded or not: the stub may not listen yet when it is online. The load is what counts.
    let _ = shepherd.get("/alpha/");

    assert_eq!(shepherd.state_of("alpha").as_deref(), Some("loaded"));
}

#[test]
fn a_config_change_through_the_shepherd_reaches_the_dog() {
    let alpha = Stub::new("alpha");
    let beta = Stub::new("beta");
    let shepherd = Shepherd::with_dog(&[&alpha]);
    shepherd.add_sheep(&beta);
    let listed = |shepherd: &Shepherd| http(shepherd.dog_port, "GET", "/v1/models", None).body;
    assert!(!listed(&shepherd).contains("beta"));

    shepherd.replace_section(&[&alpha, &beta]);

    wait_until("the dog to list the model the new section adds", || {
        listed(&shepherd).contains("beta")
    });
    // The engine heard of it too: the new model loads on a request.
    assert_eq!(shepherd.get("/beta/").status, 200);
}

#[test]
fn a_lease_holds_the_sheep_and_a_conflicting_request_is_refused() {
    let alpha = Stub::new("alpha");
    let beta = Stub::new("beta");
    let shepherd = Shepherd::with_dog(&[&alpha, &beta]);
    // The real command line, under a lease on alpha. It stays up until the test drops it.
    let _lease = Held(
        Command::new(DOG_BIN)
            .args(["run", "--model", "alpha", "--", "sleep", "30"])
            .env("PADDOCK_KEY", KEY)
            .env(
                "PADDOCK_URL",
                format!("http://127.0.0.1:{}", shepherd.dog_port),
            )
            .env("SHEP_HOME", shepherd.home())
            .env_remove("SHEP_DOG_NAME")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the command line started"),
    );
    wait_until("alpha to be loaded under the lease", || {
        shepherd.state_of("alpha").as_deref() == Some("loaded")
    });
    let status = shepherd.get("/paddock/status").body;
    assert!(status.contains("\"leases\":[{"), "{status}");

    // Beta cannot fit beside alpha, and what holds alpha has no end in sight, so it is refused
    // at once with the holder named, never queued.
    let refused = shepherd.get("/beta/");
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("alpha"), "{}", refused.body);
    assert_eq!(shepherd.state_of("alpha").as_deref(), Some("loaded"));
    assert_eq!(shepherd.state_of("beta").as_deref(), Some("unloaded"));
}

#[test]
fn the_dog_stops_a_sheep_that_crashes() {
    let alpha = Stub::new("alpha");
    let shepherd = Shepherd::with_dog(&[&alpha]);
    assert_eq!(shepherd.get("/alpha/").status, 200);
    let first = pid_of(&alpha, shepherd.home());

    let killed = Command::new("kill")
        .args(["-KILL", &first])
        .output_within(PATIENCE)
        .status;
    assert!(killed.success(), "could not kill the sheep's process");

    wait_until(
        "the dog to see the crash and mark the model unloaded",
        || shepherd.state_of("alpha").as_deref() == Some("unloaded"),
    );
    // And it is a model again, not a dead entry: the next request starts a fresh process.
    let again = shepherd.get("/alpha/");
    assert_eq!(again.status, 200, "{}", again.body);
    assert_ne!(pid_of(&alpha, shepherd.home()), first);
}

#[test]
fn a_stop_signal_during_start_up_exits_cleanly() {
    // A socket that accepts and never answers, so the dog is stuck in its handshake with the
    // shepherd. With no socket the dog exits 1 at once and the test would not reach it.
    let home = tempfile::tempdir().expect("a temporary $SHEP_HOME");
    fs::create_dir(home.path().join("run")).expect("run dir");
    let silent = std::os::unix::net::UnixListener::bind(home.path().join("run/shep.sock"))
        .expect("a socket");
    silent.set_nonblocking(true).expect("non-blocking");
    let err = fs::File::create(home.path().join("dog.err")).expect("dog.err");
    let mut dog = Held(
        Command::new(DOG_BIN)
            .env("SHEP_HOME", home.path())
            .env_remove("SHEP_DOG_NAME")
            .stdout(Stdio::null())
            .stderr(Stdio::from(err))
            .spawn()
            .expect("the dog started"),
    );
    // Printed after the stop handler is installed and before the dog connects.
    wait_until("the dog to announce it is unadopted", || {
        fs::read_to_string(home.path().join("dog.err"))
            .is_ok_and(|text| text.contains("$SHEP_DOG_NAME is not set"))
    });
    // Held open and unread, so the dog has connected and waits on a reply that never comes.
    let mut handshake = None;
    wait_until("the dog to connect to the shepherd's socket", || {
        handshake = silent.accept().ok();
        handshake.is_some()
    });

    let signalled = Command::new("kill")
        .args(["-TERM", &dog.0.id().to_string()])
        .output_within(PATIENCE)
        .status;
    assert!(signalled.success());

    let status = dog.wait_for_exit();
    assert!(
        status.success(),
        "a stop during start-up must end the dog cleanly, not by the default disposition: {status}"
    );
    drop(handshake);
}
