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

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use shep_client::{
    Client,
    shep_core::protocol::{Request, Response},
};

/// This crate's own binary, as cargo built it for this test run.
const DOG_BIN: &str = env!("CARGO_BIN_EXE_shep-paddock");

/// The name the dog is adopted under, and so the `[paddock]` section it reads.
const DOG_NAME: &str = "paddock";

/// The one client's key.
const KEY: &str = "integration-key";

/// How long any poll gets before the test fails. Generous: these tests boot a daemon, and a
/// contended machine is slow rather than broken.
const PATIENCE: Duration = Duration::from_secs(60);

/// The `shep` binary under test.
///
/// # Panics
/// If `$SHEP_BIN` is unset or does not name a file. Loudly, rather than skipping: a tier that
/// quietly does nothing is the failure this file exists to avoid.
fn shep_bin() -> PathBuf {
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

/// A loopback port nothing is listening on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port()
}

/// Poll `ready` until it answers true, or fail with `what`.
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
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
trait OutputWithin {
    /// Runs the command to its end and returns its output.
    ///
    /// # Panics
    /// If it does not start, or is still running after `limit`, when it is killed first. A
    /// descendant that keeps a pipe open after the command exits is given a second to close it,
    /// and what was read by then is returned.
    fn output_within(&mut self, limit: Duration) -> Output;
}

impl OutputWithin for Command {
    fn output_within(&mut self, limit: Duration) -> Output {
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the command started");
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
                        .expect("pipe lock")
                        .extend_from_slice(&buf[..read]);
                }
                let _ = done.send(());
            });
            (bytes, closed)
        };
        let (stdout, stdout_closed) = collected(Box::new(child.stdout.take().expect("piped")));
        let (stderr, stderr_closed) = collected(Box::new(child.stderr.take().expect("piped")));
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = child.try_wait().expect("an exit status") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{:?} still running after {limit:?}", self);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = stdout_closed.recv_timeout(Duration::from_secs(1));
        let _ = stderr_closed.recv_timeout(Duration::from_secs(1));
        let take = |bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
            std::mem::take(&mut *bytes.lock().expect("pipe lock"))
        };
        Output {
            status,
            stdout: take(stdout),
            stderr: take(stderr),
        }
    }
}

/// A process kept for the length of a test and killed on drop.
struct Held(Child);

impl Held {
    /// Ask the process to stop with TERM, and kill it if it has not gone after a few seconds.
    ///
    /// TERM first because `shep-paddock run` passes it on to its command: a kill would leave the
    /// command behind.
    fn stop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .output_within(PATIENCE);
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
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
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
struct Answer {
    status: u16,
    body: String,
}

/// Send one request to `port` on loopback and read the whole answer.
fn http(port: u16, method: &str, path: &str, key: Option<&str>) -> Answer {
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

/// One stub sheep: the model it serves and where.
struct Stub {
    name: &'static str,
    port: u16,
    /// Whether the model has a ready check, or loads once the sheep is online.
    ready: bool,
}

impl Stub {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            port: free_port(),
            ready: true,
        }
    }

    fn without_ready(name: &'static str) -> Self {
        Self {
            ready: false,
            ..Self::new(name)
        }
    }

    fn pid_file(&self, home: &Path) -> PathBuf {
        home.join(format!("{}.pid", self.name))
    }

    /// The model's table. Each takes 600M of a host that has 1000M, so the two cannot be loaded
    /// together.
    fn model(&self, home: &Path) -> String {
        let name = self.name;
        let port = self.port;
        let pid = self.pid_file(home).display().to_string();
        // Without a check, a load that never hears `online` fails well inside the test's patience.
        let ready = if self.ready {
            "ready = { path = \"/\" }"
        } else {
            "load_timeout = \"20s\""
        };
        format!(
            "[models.{name}]\n\
             backend = {{ sheep = \"{name}\", env = {{ PORT = \"{port}\", PIDFILE = \"{pid}\" }} }}\n\
             url = \"http://127.0.0.1:{port}\"\n\
             {ready}\n\
             prefix = \"/{name}\"\n\
             vram = \"600M\"\n\
             idle = \"1h\"\n"
        )
    }
}

/// One shepherd in its own temporary `$SHEP_HOME`, killed on drop, with the dog adopted into it.
struct Shepherd {
    home: tempfile::TempDir,
    shep: PathBuf,
    dog_port: u16,
}

impl Shepherd {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("a temporary $SHEP_HOME");
        // A unix socket path is bounded by the kernel: 104 bytes on macOS, 108 on Linux.
        let socket = home.path().join("run/shep.sock");
        assert!(
            socket.as_os_str().len() < 100,
            "$TMPDIR is too deep for a unix socket here: {} is {} bytes. Run with a shorter TMPDIR.",
            socket.display(),
            socket.as_os_str().len()
        );
        Self {
            home,
            shep: shep_bin(),
            dog_port: free_port(),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    /// Run one `shep` command against this home.
    ///
    /// `SHEP_HOME` goes in the environment as well as `--home`: `shep adopt` spawns the binary it
    /// vets with this environment, and a missing one would point that spawn at the real shepherd.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.shep)
            .args(args)
            .arg("--home")
            .arg(self.home())
            .env("SHEP_HOME", self.home())
            .output_within(PATIENCE)
    }

    /// Run one `shep` command and require it to succeed.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "shep {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Write this home's `dogs.toml`, where the dog's `[paddock]` section lives.
    fn write_config(&self, models: &[&Stub]) {
        fs::write(self.home().join("dogs.toml"), self.config(models)).expect("dogs.toml");
    }

    /// The dog's section, as the shepherd hands it over: no `[paddock]` header.
    fn section(&self, models: &[&Stub]) -> String {
        let mut text = format!(
            "listen = \"127.0.0.1:{}\"\n\
             [host]\n\
             vram = \"1000M\"\n\
             ram = \"1000M\"\n\
             [[clients]]\n\
             name = \"tester\"\n\
             key = \"{KEY}\"\n",
            self.dog_port
        );
        for stub in models {
            text.push_str(&stub.model(self.home()));
        }
        text
    }

    /// The whole of `dogs.toml`: the section's lines under `[paddock]` names.
    fn config(&self, models: &[&Stub]) -> String {
        let mut out = String::from("[paddock]\n");
        out.push_str(&format!("listen = \"127.0.0.1:{}\"\n", self.dog_port));
        out.push_str(&format!(
            "[paddock.host]\nvram = \"1000M\"\nram = \"1000M\"\n\
             [[paddock.clients]]\nname = \"tester\"\nkey = \"{KEY}\"\n"
        ));
        for stub in models {
            out.push_str(
                &stub
                    .model(self.home())
                    .replace("[models.", "[paddock.models."),
            );
        }
        out
    }

    /// Register a stub sheep, stopped. It serves `$PORT` and records its pid in `$PIDFILE`, both
    /// set by the dog from the model's `env` before each start.
    fn add_sheep(&self, stub: &Stub) {
        let script = self.home().join(format!("{}.sh", stub.name));
        fs::write(
            &script,
            "#!/bin/sh\necho $$ > \"$PIDFILE\"\nexec python3 -m http.server \"$PORT\" --bind 127.0.0.1\n",
        )
        .expect("a script");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        self.ok(&[
            "add",
            script.to_str().expect("a path"),
            "--name",
            stub.name,
            "--style",
            "bare",
        ]);
    }

    /// Adopt the dog, which starts it, and wait until it answers.
    fn adopt_dog(&self) {
        self.ok(&["adopt", DOG_BIN, "--name", DOG_NAME, "--style", "bare"]);
        let port = self.dog_port;
        wait_until("the dog to serve", || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
    }

    /// Boot the shepherd with `stubs` registered and the dog adopted over them.
    fn with_dog(stubs: &[&Stub]) -> Self {
        let shepherd = Self::new();
        shepherd.write_config(stubs);
        for stub in stubs {
            shepherd.add_sheep(stub);
        }
        shepherd.adopt_dog();
        shepherd
    }

    fn get(&self, path: &str) -> Answer {
        http(self.dog_port, "GET", path, Some(KEY))
    }

    /// Replace the whole `[paddock]` section the way lookout's config pane does, which is the
    /// shepherd's own write and so the one that tells a running dog.
    fn replace_section(&self, models: &[&Stub]) {
        let socket = self.home().join("run/shep.sock");
        let toml = self.section(models);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let client = Client::connect(&socket).await.expect("the control socket");
            let reply = client
                .request(Request::SetDogConfig {
                    name: DOG_NAME.to_owned(),
                    toml: toml.into(),
                })
                .await
                .expect("a reply");
            assert!(
                matches!(reply, Response::DogConfigSet { .. }),
                "the shepherd refused the section: {reply:?}"
            );
        });
    }

    /// The state of `model` in the dog's status, or `None` when it is not listed.
    fn state_of(&self, model: &str) -> Option<String> {
        let body: serde_json::Value =
            serde_json::from_str(&self.get("/paddock/status").body).ok()?;
        body["models"]
            .as_array()?
            .iter()
            .find(|row| row["model"] == model)
            .and_then(|row| row["state"].as_str().map(str::to_owned))
    }
}

impl Drop for Shepherd {
    fn drop(&mut self) {
        // A failed test gets the dog's own words, which is where the reason is.
        if std::thread::panicking() {
            let logs = self.home().join("logs");
            for name in [
                format!("{DOG_NAME}-0-err.log"),
                format!("{DOG_NAME}-0-out.log"),
            ] {
                let text = fs::read_to_string(logs.join(&name)).unwrap_or_default();
                eprintln!("--- {name}\n{text}");
            }
        }
        // Before the tempdir goes, so the daemon is not holding a home that no longer exists.
        // Failures are ignored: a test that already failed must report its own reason.
        let _ = self.run(&["stop", "all", "--style", "bare"]);
        let _ = self.run(&["kill", "--style", "bare"]);
    }
}

fn pid_of(stub: &Stub, home: &Path) -> String {
    fs::read_to_string(stub.pid_file(home))
        .expect("the sheep wrote its pid")
        .trim()
        .to_owned()
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
    // A socket nobody answers on, so the dog is stuck in its handshake with the shepherd.
    let home = tempfile::tempdir().expect("a temporary $SHEP_HOME");
    fs::create_dir(home.path().join("run")).expect("run dir");
    let _silent = std::os::unix::net::UnixListener::bind(home.path().join("run/shep.sock"))
        .expect("a socket");
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
}
