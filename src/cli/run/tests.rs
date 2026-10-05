//! `run` against a fake dog. These tests use real time, not a paused clock: each one spawns a
//! process, and the fake dog is a real loopback socket. Every await is bounded by `LIMIT`, and
//! the retry between attaches is shortened to milliseconds through the link.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
    time::timeout,
};

use super::run;
use crate::{
    cli::{Forward, Link, RunArgs},
    test_support::{FakeHttp, Seen, fake_http},
};

// Far past a command of a third of a second and a few attaches.
const LIMIT: Duration = Duration::from_secs(20);

const QUEUED: &str = "{\"queued\":{\"reason\":\"iq2_xs is held by bench-01\",\"estimate\":null}}\n";
const QUEUED_AGAIN: &str = "{\"queued\":{\"reason\":\"iq2_xs is unloading\",\"estimate\":null}}\n";
const BEAT: &str = "{\"heartbeat\":{}}\n";
const GRANTED: &str = "{\"granted\":{\"id\":\"L1\",\"reconnect\":\"60s\"}}\n";
const GRANTED_BRIEFLY: &str = "{\"granted\":{\"id\":\"L1\",\"reconnect\":\"100ms\"}}\n";
const ENDED: &str = "{\"ended\":{\"why\":\"expired\"}}\n";
const RELEASED: (u16, &str) = (204, "");

/// A signal source that never fires.
fn quiet() -> UnboundedReceiver<Forward> {
    unbounded_channel().1
}

fn args(command: &[&str]) -> RunArgs {
    RunArgs {
        model: "iq2_xs".to_owned(),
        expected: None,
        note: None,
        interactive: false,
        command: command.iter().map(|word| (*word).to_owned()).collect(),
    }
}

fn link(url: String) -> Link {
    Link {
        url,
        key: "k-bench".to_owned(),
        retry: Duration::from_millis(10),
        silence: Duration::from_secs(45),
    }
}

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    match timeout(LIMIT, future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out: {what}"),
    }
}

/// Runs `command` against a dog whose lease stream says `stream` and whose attach answers
/// `attach`, returning the exit code, what went to stderr and the server to inspect.
async fn go(
    stream: &'static str,
    attach: (u16, &'static str),
    command: &[&str],
) -> (u8, String, FakeHttp) {
    let routes = vec![
        ("POST", "/paddock/leases", vec![(200, stream)]),
        ("POST", "/paddock/leases/L1/attach", vec![attach]),
        ("DELETE", "/paddock/leases/L1", vec![RELEASED]),
    ];
    let (url, server) = fake_http(routes);
    let mut err = Vec::new();
    let code = bounded(
        "the run",
        run(&link(url), &args(command), &mut err, &mut quiet()),
    )
    .await;
    (code, String::from_utf8_lossy(&err).into_owned(), server)
}

fn count(server: &FakeHttp, method: &str, path: &str) -> usize {
    server
        .seen()
        .iter()
        .filter(|seen| seen.method == method && seen.path == path)
        .count()
}

fn took(server: &FakeHttp) -> Seen {
    let all = server.seen();
    let Some(first) = all.first() else {
        panic!("the dog saw nothing");
    };
    first.clone()
}

#[tokio::test]
async fn run_releases_after_the_command_exits() {
    let stream: &'static str = Box::leak(format!("{GRANTED}{BEAT}").into_boxed_str());
    let (code, said, server) = go(stream, (200, GRANTED), &["sh", "-c", "exit 0"]).await;
    assert_eq!(code, 0, "{said}");
    let released: Vec<_> = server
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "DELETE")
        .collect();
    assert_eq!(released.len(), 1, "one release: {:?}", server.seen());
    assert_eq!(released[0].path, "/paddock/leases/L1");
    assert_eq!(released[0].authorization.as_deref(), Some("Bearer k-bench"));
    assert_eq!(server.seen().last().expect("seen").method, "DELETE");
}

#[tokio::test]
async fn run_exits_with_the_commands_status() {
    let (code, _, server) = go(GRANTED, (200, GRANTED), &["sh", "-c", "exit 3"]).await;
    assert_eq!(code, 3);
    assert_eq!(count(&server, "DELETE", "/paddock/leases/L1"), 1);
}

#[tokio::test]
async fn a_command_killed_by_a_signal_exits_128_plus_the_signal() {
    let (code, _, _) = go(GRANTED, (200, GRANTED), &["sh", "-c", "kill -TERM $$"]).await;
    assert_eq!(code, 143);
}

#[tokio::test]
async fn the_command_runs_with_the_lease_id_in_its_environment() {
    let (code, said, _) = go(
        GRANTED,
        (200, GRANTED),
        &["sh", "-c", "test \"$PADDOCK_LEASE\" = L1"],
    )
    .await;
    assert_eq!(code, 0, "PADDOCK_LEASE was not L1: {said}");
}

#[tokio::test]
async fn a_command_that_cannot_start_exits_127_and_still_releases() {
    let (code, said, server) = go(GRANTED, (200, GRANTED), &["/nonexistent/paddock-test"]).await;
    assert_eq!(code, 127);
    assert!(said.contains("/nonexistent/paddock-test"), "{said}");
    assert_eq!(count(&server, "DELETE", "/paddock/leases/L1"), 1);
}

#[tokio::test]
async fn each_queued_reason_goes_to_stderr_in_order() {
    let stream: &'static str =
        Box::leak(format!("{QUEUED}{BEAT}{QUEUED_AGAIN}{GRANTED}").into_boxed_str());
    let (code, said, _) = go(stream, (200, GRANTED), &["sh", "-c", "exit 0"]).await;
    assert_eq!(code, 0);
    let first = said
        .find("iq2_xs is held by bench-01")
        .expect("first reason");
    let second = said.find("iq2_xs is unloading").expect("second reason");
    assert!(first < second, "{said}");
}

#[tokio::test]
async fn run_reattaches_after_a_broken_stream() {
    // The stream ends after the grant without saying so, which is a break, and the command
    // outlives several retries.
    let (code, said, server) =
        go(GRANTED, (200, GRANTED), &["sh", "-c", "sleep 0.3; exit 5"]).await;
    assert_eq!(code, 5, "{said}");
    let attaches: Vec<_> = server
        .seen()
        .into_iter()
        .filter(|seen| seen.path == "/paddock/leases/L1/attach")
        .collect();
    assert!(
        attaches.len() >= 2,
        "attached again and again: {attaches:?}"
    );
    assert_eq!(attaches[0].method, "POST");
    assert_eq!(attaches[0].authorization.as_deref(), Some("Bearer k-bench"));
    assert_eq!(count(&server, "DELETE", "/paddock/leases/L1"), 1);
    assert!(!said.contains("lost"), "the lease was kept: {said}");
}

#[tokio::test]
async fn run_lets_the_command_finish_when_the_lease_is_lost() {
    let stream: &'static str = Box::leak(format!("{GRANTED}{ENDED}").into_boxed_str());
    let (code, said, server) = go(stream, (404, ""), &["sh", "-c", "sleep 0.3; exit 4"]).await;
    assert_eq!(
        code, 4,
        "the command finished and its status is the exit code"
    );
    assert!(said.contains("the lease ended (expired)"), "{said}");
    assert_eq!(
        count(&server, "DELETE", "/paddock/leases/L1"),
        0,
        "nothing to release"
    );
}

#[tokio::test]
async fn an_attach_the_dog_refuses_means_the_lease_is_gone() {
    let (code, said, server) = go(GRANTED, (404, ""), &["sh", "-c", "sleep 0.3; exit 6"]).await;
    assert_eq!(code, 6);
    assert!(said.contains("the lease is gone"), "{said}");
    assert_eq!(count(&server, "DELETE", "/paddock/leases/L1"), 0);
}

#[tokio::test]
async fn reattaching_stops_when_the_reconnect_time_runs_out() {
    let (code, said, server) = go(
        GRANTED_BRIEFLY,
        (503, ""),
        &["sh", "-c", "sleep 0.6; exit 7"],
    )
    .await;
    assert_eq!(code, 7, "the command is never killed");
    assert!(said.contains("reconnect time ran out"), "{said}");
    let attached = count(&server, "POST", "/paddock/leases/L1/attach");
    assert!(
        (2..=30).contains(&attached),
        "tried a few times, then stopped: {attached}"
    );
}

#[tokio::test]
async fn a_refusal_exits_75_with_the_reason() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let flag = dir.path().join("ran");
    let script = format!("touch {}", flag.display());
    let stream = "{\"refused\":{\"error\":\"busy\",\"model\":\"iq2_xs\",\"reason\":\"iq2_xs is held by bench-01 since 2026-10-04T10:00:00Z\",\"expected_until\":\"2026-10-04T18:00:00Z\"}}\n";
    let (code, said, server) = go(stream, (404, ""), &["sh", "-c", &script]).await;
    assert_eq!(code, 75, "{said}");
    assert!(said.contains("iq2_xs is held by bench-01 since"), "{said}");
    assert!(said.contains("2026-10-04T18:00:00Z"), "{said}");
    assert!(!flag.exists(), "the command never ran");
    assert_eq!(count(&server, "DELETE", "/paddock/leases/L1"), 0);
}

#[tokio::test]
async fn a_failed_load_exits_1_with_the_reason() {
    let stream = "{\"failed\":{\"error\":\"failed\",\"model\":\"iq2_xs\",\"reason\":\"the sheep would not start\"}}\n";
    let (code, said, _) = go(stream, (404, ""), &["true"]).await;
    assert_eq!(code, 1);
    assert!(said.contains("the sheep would not start"), "{said}");
}

#[tokio::test]
async fn a_stream_that_breaks_before_the_grant_exits_75() {
    let (code, said, _) = go(QUEUED, (404, ""), &["true"]).await;
    assert_eq!(code, 75, "{said}");
    assert!(said.contains("before the lease was granted"), "{said}");
}

#[tokio::test]
async fn a_lease_that_ends_before_the_grant_exits_1() {
    let (code, said, _) = go(ENDED, (404, ""), &["true"]).await;
    assert_eq!(code, 1);
    assert!(said.contains("before it was granted"), "{said}");
}

#[tokio::test]
async fn an_answer_that_is_not_a_stream_exits_1_with_the_status() {
    let (url, _server) = fake_http(vec![(
        "POST",
        "/paddock/leases",
        vec![(422, r#"{"error":"never_fits"}"#)],
    )]);
    let mut err = Vec::new();
    let code = bounded(
        "the run",
        run(&link(url), &args(&["true"]), &mut err, &mut quiet()),
    )
    .await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(code, 1);
    assert!(
        said.contains("422") && said.contains("never_fits"),
        "{said}"
    );
    assert!(!said.contains("k-bench"), "{said}");
}

#[tokio::test]
async fn an_unreachable_dog_exits_1() {
    let mut err = Vec::new();
    let down = link("http://127.0.0.1:1".to_owned());
    let code = bounded(
        "the run",
        run(&down, &args(&["true"]), &mut err, &mut quiet()),
    )
    .await;
    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&err).contains("127.0.0.1:1"));
}

#[tokio::test]
async fn the_lease_is_taken_with_what_the_arguments_say() {
    let (url, server) = fake_http(vec![
        ("POST", "/paddock/leases", vec![(200, GRANTED)]),
        ("POST", "/paddock/leases/L1/attach", vec![(200, GRANTED)]),
        ("DELETE", "/paddock/leases/L1", vec![RELEASED]),
    ]);
    let mut full = args(&["true"]);
    full.expected = Some("8h".to_owned());
    full.note = Some("strata h2h run 3".to_owned());
    full.interactive = true;
    let mut err = Vec::new();
    bounded("the run", run(&link(url), &full, &mut err, &mut quiet())).await;
    let first = took(&server);
    assert_eq!(
        (first.method.as_str(), first.path.as_str()),
        ("POST", "/paddock/leases")
    );
    assert_eq!(first.authorization.as_deref(), Some("Bearer k-bench"));
    let body: Value = serde_json::from_str(&first.body).expect("a JSON body");
    assert_eq!(
        body,
        json!({
            "model": "iq2_xs",
            "priority": "interactive",
            "hold": "connection",
            "expected": "8h",
            "note": "strata h2h run 3",
        })
    );
}

#[tokio::test]
async fn a_plain_run_asks_for_a_batch_lease_and_leaves_out_what_it_was_not_given() {
    let (_, _, server) = go(GRANTED, (200, GRANTED), &["true"]).await;
    let body: Value = serde_json::from_str(&took(&server).body).expect("a JSON body");
    assert_eq!(
        body,
        json!({ "model": "iq2_xs", "priority": "batch", "hold": "connection" })
    );
}

#[tokio::test]
async fn the_key_never_reaches_stderr() {
    for stream in [GRANTED, ENDED, QUEUED] {
        let (_, said, _) = go(stream, (404, ""), &["sh", "-c", "exit 1"]).await;
        assert!(!said.contains("k-bench"), "{said}");
    }
}

/// A command that leaves with 0 once `signal` reaches it: it says it is ready, then waits. It
/// gives up with 7 after ten seconds so a test that fails leaves no process behind.
fn waits_for(signal: &str, ready: &std::path::Path) -> String {
    format!(
        "trap 'exit 0' {signal}; touch {}; n=0; while [ $n -lt 200 ]; do sleep 0.05; n=$((n+1)); done; exit 7",
        ready.display()
    )
}

async fn forwarded(signal: Forward, name: &str) {
    let dir = tempfile::tempdir().expect("scratch directory");
    let ready = dir.path().join("ready");
    let script = waits_for(name, &ready);
    let (url, server) = fake_http(vec![
        ("POST", "/paddock/leases", vec![(200, GRANTED)]),
        ("POST", "/paddock/leases/L1/attach", vec![(200, GRANTED)]),
        ("DELETE", "/paddock/leases/L1", vec![RELEASED]),
    ]);
    let (sender, mut signals) = unbounded_channel();
    let mut err = Vec::new();
    let held = link(url);
    let command = args(&["sh", "-c", &script]);
    let running = run(&held, &command, &mut err, &mut signals);
    let sending = async {
        // The trap is set before the file appears, so the signal cannot arrive too early.
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            count(&server, "DELETE", "/paddock/leases/L1"),
            0,
            "held while it runs"
        );
        sender.send(signal).expect("the run listens");
    };
    let (code, ()) = bounded("the run", async { tokio::join!(running, sending) }).await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(code, 0, "the command got {name} and left: {said}");
    assert_eq!(
        count(&server, "DELETE", "/paddock/leases/L1"),
        1,
        "released after it exited"
    );
    assert_eq!(server.seen().last().expect("seen").method, "DELETE");
}

#[tokio::test]
async fn a_term_sent_to_the_wrapper_reaches_the_command_and_the_lease_is_released_after() {
    forwarded(Forward::Terminate, "TERM").await;
}

#[tokio::test]
async fn a_hup_sent_to_the_wrapper_reaches_the_command_and_the_lease_is_released_after() {
    forwarded(Forward::Hangup, "HUP").await;
}

/// A dog that sends `first`, an HTTP chunk, as the answer to a take and then says nothing more
/// while the connection stays open, which fake_http cannot do. Every other request is answered
/// at once: attach with a 404 and anything else with a 204. Returns the base url and the
/// request lines seen.
async fn silent_dog(first: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&lines);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buffer[..read]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                seen.lock().expect("seen lock").push(line.clone());
                let answer = if line.starts_with("POST /paddock/leases ") {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\n\
                         transfer-encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n",
                        first.len()
                    )
                } else if line.contains("/attach") {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_owned()
                } else {
                    "HTTP/1.1 204 No Content\r\n\r\n".to_owned()
                };
                let _ = stream.write_all(answer.as_bytes()).await;
                // Open and silent, for as long as the test runs.
                core::future::pending::<()>().await;
            });
        }
    });
    (url, lines)
}

// Real time: the silence is the thing under test, so it is set to a tenth of a second and the
// dog really is quiet for that long. In production it is 45 s.
#[tokio::test]
async fn a_stream_that_goes_silent_counts_as_broken_and_is_attached_again() {
    let (url, lines) = silent_dog(GRANTED).await;
    let mut held = link(url);
    held.silence = Duration::from_millis(100);
    let mut err = Vec::new();
    let command = args(&["sh", "-c", "sleep 1; exit 8"]);
    let code = bounded("the run", run(&held, &command, &mut err, &mut quiet())).await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(code, 8, "{said}");
    assert!(said.contains("the connection to the dog broke"), "{said}");
    let seen = lines.lock().expect("seen lock").clone();
    assert!(
        seen.iter()
            .any(|line| line.starts_with("POST /paddock/leases/L1/attach")),
        "attached again: {seen:?}"
    );
}

#[tokio::test]
async fn a_dog_that_goes_silent_before_the_grant_exits_75() {
    let (url, _lines) = silent_dog(QUEUED).await;
    let mut held = link(url);
    held.silence = Duration::from_millis(100);
    let mut err = Vec::new();
    let code = bounded(
        "the run",
        run(&held, &args(&["true"]), &mut err, &mut quiet()),
    )
    .await;
    assert_eq!(code, 75);
    assert!(String::from_utf8_lossy(&err).contains("before the lease was granted"));
}
