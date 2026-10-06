//! The integration tier's harness: a shepherd in a temporary home with the dog adopted into it,
//! and the stub sheep it runs.

use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use shep_client::{
    Client,
    shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec},
};

use crate::bounded::{
    Answer, DOG_BIN, DOG_NAME, KEY, OutputWithin, PATIENCE, free_port, http, shep_bin, wait_until,
};

/// One stub sheep: the model it serves and where.
pub(crate) struct Stub {
    pub(crate) name: &'static str,
    pub(crate) port: u16,
    /// Whether the model has a ready check, or loads once the sheep is online.
    pub(crate) ready: bool,
}

impl Stub {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            port: free_port(),
            ready: true,
        }
    }

    pub(crate) fn without_ready(name: &'static str) -> Self {
        Self {
            ready: false,
            ..Self::new(name)
        }
    }

    pub(crate) fn pid_file(&self, home: &Path) -> PathBuf {
        home.join(format!("{}.pid", self.name))
    }

    /// The model's table. Each takes 600M of a host that has 1000M, so the two cannot be loaded
    /// together.
    pub(crate) fn model(&self, home: &Path) -> String {
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
pub(crate) struct Shepherd {
    pub(crate) home: tempfile::TempDir,
    pub(crate) shep: PathBuf,
    pub(crate) dog_port: u16,
}

impl Shepherd {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn home(&self) -> &Path {
        self.home.path()
    }

    /// Run one `shep` command against this home.
    ///
    /// # Panics
    /// As [`OutputWithin::output_within`].
    #[track_caller]
    pub(crate) fn run(&self, args: &[&str]) -> Output {
        self.command(args).output_within(PATIENCE)
    }

    /// One `shep` command against this home, not yet run.
    ///
    /// `SHEP_HOME` goes in the environment as well as `--home`: `shep adopt` spawns the binary it
    /// vets with this environment, and a missing one would point that spawn at the real shepherd.
    pub(crate) fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.shep);
        command
            .args(args)
            .arg("--home")
            .arg(self.home())
            .env("SHEP_HOME", self.home());
        command
    }

    /// Run one `shep` command and require it to succeed.
    pub(crate) fn ok(&self, args: &[&str]) -> String {
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
    pub(crate) fn write_config(&self, models: &[&Stub]) {
        fs::write(self.home().join("dogs.toml"), self.config(models)).expect("dogs.toml");
    }

    /// The dog's section, as the shepherd hands it over: no `[paddock]` header.
    pub(crate) fn section(&self, models: &[&Stub]) -> String {
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
    pub(crate) fn config(&self, models: &[&Stub]) -> String {
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
    pub(crate) fn add_sheep(&self, stub: &Stub) {
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
    pub(crate) fn adopt_dog(&self) {
        self.ok(&["adopt", DOG_BIN, "--name", DOG_NAME, "--style", "bare"]);
        let port = self.dog_port;
        wait_until("the dog to serve", || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
    }

    /// Boot the shepherd with `stubs` registered and the dog adopted over them.
    pub(crate) fn with_dog(stubs: &[&Stub]) -> Self {
        let shepherd = Self::new();
        shepherd.write_config(stubs);
        for stub in stubs {
            shepherd.add_sheep(stub);
        }
        shepherd.adopt_dog();
        shepherd
    }

    pub(crate) fn get(&self, path: &str) -> Answer {
        http(self.dog_port, "GET", path, Some(KEY))
    }

    /// Replace the whole `[paddock]` section the way lookout's config pane does, which is the
    /// shepherd's own write and so the one that tells a running dog.
    pub(crate) fn replace_section(&self, models: &[&Stub]) {
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

    /// What the shepherd answers `Describe` of the whole flock with, as the dog's survey asks it.
    pub(crate) fn describe_all(&self) -> Result<Vec<ProcessInfo>, String> {
        let socket = self.home().join("run/shep.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let client = Client::connect(&socket).await.expect("the control socket");
            let asked = Request::Describe {
                selector: SelectorSpec::All,
            };
            match client.request(asked).await {
                Ok(Response::Described(flock)) => Ok(flock),
                Ok(other) => Err(format!("an unexpected {}", other.name())),
                Err(err) => Err(err.to_string()),
            }
        })
    }

    /// The state of `model` in the dog's status, or `None` when it is not listed.
    pub(crate) fn state_of(&self, model: &str) -> Option<String> {
        let body: serde_json::Value =
            serde_json::from_str(&self.get("/paddock/status").body).ok()?;
        body["models"]
            .as_array()?
            .iter()
            .find(|row| row["model"] == model)
            .and_then(|row| row["state"].as_str().map(str::to_owned))
    }

    /// Whether the dog's status marks `model` a stray, or `None` when it is not listed.
    pub(crate) fn stray_of(&self, model: &str) -> Option<bool> {
        let body: serde_json::Value =
            serde_json::from_str(&self.get("/paddock/status").body).ok()?;
        body["models"]
            .as_array()?
            .iter()
            .find(|row| row["model"] == model)
            .and_then(|row| row["stray"].as_bool())
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
        let _ = self
            .command(&["stop", "all", "--style", "bare"])
            .try_output_within(PATIENCE);
        let _ = self
            .command(&["kill", "--style", "bare"])
            .try_output_within(PATIENCE);
    }
}

pub(crate) fn pid_of(stub: &Stub, home: &Path) -> String {
    fs::read_to_string(stub.pid_file(home))
        .expect("the sheep wrote its pid")
        .trim()
        .to_owned()
}
