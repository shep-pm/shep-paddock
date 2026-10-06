//! The integration tier's bounded waits, commands and requests, so no test hangs on one.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

/// This crate's own binary, as cargo built it for this test run.
pub(crate) const DOG_BIN: &str = env!("CARGO_BIN_EXE_shep-paddock");

/// The name the dog is adopted under, and so the `[paddock]` section it reads.
pub(crate) const DOG_NAME: &str = "paddock";

/// The one client's key.
pub(crate) const KEY: &str = "integration-key";

/// How long any poll gets before the test fails. Generous: these tests boot a daemon, and a
/// contended machine is slow rather than broken.
pub(crate) const PATIENCE: Duration = Duration::from_secs(60);

/// The `shep` binary under test.
///
/// # Panics
/// If `$SHEP_BIN` is unset or does not name a file. Loudly, rather than skipping: a tier that
/// quietly does nothing is the failure this file exists to avoid.
pub(crate) fn shep_bin() -> PathBuf {
    let raw = std::env::var("SHEP_BIN").expect(
        "the integration tier needs $SHEP_BIN pointing at a built shep binary, for example \
         SHEP_BIN=\"$(command -v shep)\"",
    );
    let path = PathBuf::from(raw);
    assert!(
        path.is_file(),
        "$SHEP_BIN does not name a file: {}",
        path.display()
    );
    path
}

/// A loopback port nothing is listening on right now, and not one this run handed out before.
///
/// A port is only free until its listener drops, so the OS may hand the same one to two tests
/// running side by side before either has bound it. Remembering what was given out closes that
/// within this process; another process taking one in the gap is still possible.
pub(crate) fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    let mut given = GIVEN.lock().expect("port list lock");
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("its address")
            .port();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

/// Poll `ready` until it answers true, or fail with `what`.
pub(crate) fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

/// What a command printed, read while it runs so a full pipe cannot stall it.
pub(crate) trait OutputWithin {
    /// Runs the command to its end and returns its output.
    ///
    /// # Panics
    /// If it does not start, or is still running after `limit`, when it is killed first. A
    /// descendant that keeps a pipe open after the command exits is given a second to close it,
    /// and what was read by then is returned.
    fn output_within(&mut self, limit: Duration) -> Output;

    /// As [`OutputWithin::output_within`], with `None` where that panics, for a drop to call.
    fn try_output_within(&mut self, limit: Duration) -> Option<Output>;
}

impl OutputWithin for Command {
    #[track_caller]
    fn output_within(&mut self, limit: Duration) -> Output {
        self.try_output_within(limit).unwrap_or_else(|| {
            panic!("{self:?} did not start, or was still running after {limit:?}")
        })
    }

    fn try_output_within(&mut self, limit: Duration) -> Option<Output> {
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let collected = |mut pipe: Box<dyn Read + Send>| {
            let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let (done, closed) = std::sync::mpsc::channel();
            let sink = std::sync::Arc::clone(&bytes);
            std::thread::spawn(move || {
                let mut buf = [0_u8; 4096];
                while let Ok(read) = pipe.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    sink.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(&buf[..read]);
                }
                let _ = done.send(());
            });
            (bytes, closed)
        };
        let (stdout, stdout_closed) = collected(Box::new(child.stdout.take()?));
        let (stderr, stderr_closed) = collected(Box::new(child.stderr.take()?));
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = stdout_closed.recv_timeout(Duration::from_secs(1));
        let _ = stderr_closed.recv_timeout(Duration::from_secs(1));
        let take = |bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
            std::mem::take(
                &mut *bytes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        };
        Some(Output {
            status,
            stdout: take(stdout),
            stderr: take(stderr),
        })
    }
}

/// A process kept for the length of a test and killed on drop.
pub(crate) struct Held(pub(crate) Child);

impl Held {
    /// Ask the process to stop with TERM, and kill it if it has not gone after a few seconds.
    ///
    /// TERM first because `shep-paddock run` passes it on to its command: a kill would leave the
    /// command behind.
    pub(crate) fn stop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .try_output_within(PATIENCE);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }

    /// Wait for the process to exit on its own and say how it did.
    ///
    /// # Panics
    /// If it is still running after [`PATIENCE`].
    pub(crate) fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Some(status) = self.0.try_wait().expect("an exit status") {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the process is still running");
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An answer from the dog: status code and body.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: String,
}

/// Send one request to `port` on loopback and read the whole answer.
pub(crate) fn http(port: u16, method: &str, path: &str, key: Option<&str>) -> Answer {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("the dog's port");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    let auth = key
        .map(|key| format!("Authorization: Bearer {key}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("a request");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("an answer");
    Answer {
        status: text
            .get(9..12)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in the answer: {text:?}")),
        body: text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned(),
    }
}
