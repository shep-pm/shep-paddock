//! Signals and silence: what `run` does when the wrapper is signalled and when the dog goes
//! quiet. Real time throughout, as in the parent module.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::{
        Notify,
        mpsc::{UnboundedReceiver, unbounded_channel},
    },
};

use super::{GRANTED, QUEUED, RELEASED, args, bounded, count, fake_http, link, quiet};
use crate::cli::{Forward, run::run};

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
    slow_dog(first, None).await
}

/// A [`silent_dog`] that sends `later` on the take's stream once told to, if it has one.
async fn slow_dog(
    first: &'static str,
    later: Option<(Arc<Notify>, &'static str)>,
) -> (String, Arc<Mutex<Vec<String>>>) {
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
            let later = later.clone();
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
                if let (Some((told, more)), true) =
                    (later, line.starts_with("POST /paddock/leases "))
                {
                    told.notified().await;
                    let chunk = format!("{:x}\r\n{more}\r\n", more.len());
                    let _ = stream.write_all(chunk.as_bytes()).await;
                }
                // Open and silent until the client hangs up.
                while matches!(stream.read(&mut buffer).await, Ok(read) if read > 0) {}
                seen.lock().expect("seen lock").push("closed".to_owned());
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

#[tokio::test]
async fn a_run_queued_for_a_lease_leaves_on_a_signal_and_never_starts_the_command() {
    for (signal, code) in [
        (Forward::Interrupt, 130),
        (Forward::Terminate, 143),
        (Forward::Hangup, 129),
    ] {
        let dir = tempfile::tempdir().expect("scratch directory");
        let flag = dir.path().join("ran");
        let script = format!("touch {}", flag.display());
        let (url, lines) = silent_dog(QUEUED).await;
        let (sender, mut signals) = unbounded_channel();
        let held = link(url);
        let command = args(&["sh", "-c", &script]);
        let mut err = Vec::new();
        let running = run(&held, &command, &mut err, &mut signals);
        let sending = async {
            // The dog has the take and the run has read its queued line, or will once this
            // signal is read after it.
            while !lines
                .lock()
                .expect("seen lock")
                .iter()
                .any(|line| line.starts_with("POST"))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            sender.send(signal).expect("the run listens");
        };
        let (exit, ()) = bounded("the run", async { tokio::join!(running, sending) }).await;
        assert_eq!(exit, code, "{signal:?}: {}", String::from_utf8_lossy(&err));
        assert!(!flag.exists(), "{signal:?}: the command never ran");
        let closed = bounded("the dog seeing the hang-up", async {
            while !lines
                .lock()
                .expect("seen lock")
                .iter()
                .any(|line| line == "closed")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        closed.await;
        assert_eq!(
            count_lines(&lines, "DELETE"),
            0,
            "nothing was granted to release"
        );
    }
}

fn count_lines(lines: &Arc<Mutex<Vec<String>>>, start: &str) -> usize {
    lines
        .lock()
        .expect("seen lock")
        .iter()
        .filter(|line| line.starts_with(start))
        .count()
}

/// A dog that accepts connections and never answers one, and the receiver that hears each time
/// it has read some of a request.
async fn mute_dog() -> (String, UnboundedReceiver<()>) {
    let (heard, requests) = unbounded_channel();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let heard = heard.clone();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 1024];
                while matches!(stream.read(&mut buffer).await, Ok(read) if read > 0) {
                    let _ = heard.send(());
                }
            });
        }
    });
    (url, requests)
}

#[tokio::test]
async fn a_dog_that_never_answers_the_take_fails_the_run_instead_of_hanging_it() {
    let (url, _requests) = mute_dog().await;
    let mut held = link(url);
    held.silence = Duration::from_millis(200);
    let mut err = Vec::new();
    let code = bounded(
        "the run",
        run(&held, &args(&["true"]), &mut err, &mut quiet()),
    )
    .await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(code, 1, "{said}");
    assert!(said.contains("did not answer"), "{said}");
}

// Each ignored INT wakes the run's loop without bringing the dog's stream anything. The
// silence limit is 300 ms and the wakeups come every 50 ms until the command ends, so a
// deadline that restarts at each wakeup never expires.
#[tokio::test]
async fn wakeups_that_bring_nothing_do_not_push_the_silence_deadline_out() {
    let (url, _lines) = silent_dog(GRANTED).await;
    let mut held = link(url);
    held.silence = Duration::from_millis(300);
    let (sender, mut signals) = unbounded_channel();
    let mut err = Vec::new();
    let command = args(&["sh", "-c", "sleep 1; exit 8"]);
    let running = run(&held, &command, &mut err, &mut signals);
    let sending = async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = sender.send(Forward::Interrupt);
        }
    };
    let code = bounded("the run", async {
        tokio::select! {
            code = running => code,
            () = sending => unreachable!("the sender never ends"),
        }
    })
    .await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(code, 8, "{said}");
    assert!(said.contains("the connection to the dog broke"), "{said}");
}

/// What a run writes to its error stream, which a test can read while the run goes on.
#[derive(Clone, Default)]
struct Said(Arc<Mutex<Vec<u8>>>);

impl Said {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("said lock")).into_owned()
    }

    /// Waits until the run has said `words`.
    async fn until(&self, words: &str) {
        while !self.text().contains(words) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl std::io::Write for Said {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("said lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_signal_that_beats_an_inflight_grant_still_releases_the_lease() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let flag = dir.path().join("ran");
    let script = format!("touch {}", flag.display());
    let grant = Arc::new(Notify::new());
    let (url, lines) = slow_dog(QUEUED, Some((Arc::clone(&grant), GRANTED))).await;
    let (sender, mut signals) = unbounded_channel();
    let held = link(url);
    let command = args(&["sh", "-c", &script]);
    let said = Said::default();
    let mut err = said.clone();
    let running = run(&held, &command, &mut err, &mut signals);
    let sending = async {
        // Queued, so the signal reaches the run while it waits for the grant.
        said.until("waiting:").await;
        sender.send(Forward::Terminate).expect("the run listens");
        // Leaving, so the grant is one already on its way when the run decided.
        said.until("leaving the queue").await;
        grant.notify_one();
    };
    let (exit, ()) = bounded("the run", async { tokio::join!(running, sending) }).await;
    assert_eq!(exit, 143, "{}", said.text());
    assert!(!flag.exists(), "the command never ran");
    assert!(
        lines
            .lock()
            .expect("seen lock")
            .iter()
            .any(|line| line.starts_with("DELETE /paddock/leases/L1")),
        "released: {:?}",
        lines.lock().expect("seen lock")
    );
}

#[tokio::test]
async fn a_signal_while_the_take_is_in_flight_leaves_without_starting_the_command() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let flag = dir.path().join("ran");
    let script = format!("touch {}", flag.display());
    let (url, mut requests) = mute_dog().await;
    let held = link(url);
    let (sender, mut signals) = unbounded_channel();
    let command = args(&["sh", "-c", &script]);
    let mut err = Vec::new();
    let running = run(&held, &command, &mut err, &mut signals);
    let sending = async {
        // The dog never answers, so the take is in flight once the dog has read the request.
        requests.recv().await.expect("the dog hears the take");
        sender.send(Forward::Terminate).expect("the run listens");
    };
    let (exit, ()) = bounded("the run", async { tokio::join!(running, sending) }).await;
    let said = String::from_utf8_lossy(&err);
    assert_eq!(exit, 143, "{said}");
    assert!(said.contains("leaving the queue"), "{said}");
    assert!(!flag.exists(), "the command ran");
}
