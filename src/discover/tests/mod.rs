//! Discovery against the fake shepherd and a fake HTTP server on a real loopback socket, so these
//! tests run on real time. Every await is bounded by `LIMIT`.

use std::{path::Path, sync::Arc, time::Duration};

use tokio::time::timeout;

use super::*;
use crate::{
    book::{Found, LeaseId, Priority},
    footprint::{Footprint, Vram},
    saved::{self, SavedHold, SavedLease, SavedModel},
    test_support::{FakeShepherd, config, fake_http},
};

mod moved;
mod ollama;
mod strays;
mod survey;

// Past one listing and a ready check or two on loopback.
const LIMIT: Duration = Duration::from_secs(10);

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// The saved state as a restart reads it: written to a scratch `$SHEP_HOME`, then loaded.
///
/// Each sheep's model has a `models` entry with no placement, as the dog writes one it loaded.
fn saved_in(home: &Path, sheep: &[(&str, &str)]) -> Saved {
    let path = saved::path_in(home);
    let dogs = SavedModel {
        placement: None,
        stray: false,
    };
    let written = Saved {
        sheep: sheep
            .iter()
            .map(|(sheep, model)| ((*sheep).to_owned(), ModelName::from(*model)))
            .collect(),
        models: sheep
            .iter()
            .map(|(_, model)| (ModelName::from(*model), dogs.clone()))
            .collect(),
        ..Saved::default()
    };
    saved::store(&path, &written).expect("stored");
    saved::load(&path).expect("loaded").expect("present")
}

/// The names of the stand-ins discovery made, in order.
fn stand_ins(discovered: &Discovered) -> Vec<&str> {
    discovered
        .stand_ins
        .iter()
        .map(|model| model.name.as_str())
        .collect()
}

/// What discovery reports for `model` found at `footprint` with no placement.
fn counted(model: &str, footprint: Footprint, stray: bool) -> Found {
    Found {
        model: ModelName::from(model),
        footprint,
        placement: None,
        stray,
    }
}

async fn found(config: &Arc<Config>, shepherd: FakeShepherd, saved: &Saved) -> Discovered {
    let backends = Backends::new(shepherd, crate::outbound::http_client());
    match timeout(LIMIT, discover(config, &backends, saved)).await {
        Ok(discovered) => discovered,
        Err(_) => panic!("discovery did not finish within {LIMIT:?}"),
    }
}

/// Two models on one sheep with ready checks at `base`, and laya on a sheep of its own.
fn shared_sheep(base: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.iq2_xs]
backend = {{ sheep = "iq2_xs", env = {{ CONTEXT = "131072" }} }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
vram = "20000M"
ram = "44G"
idle = "2h"

[models.iq2_xs-256k]
backend = {{ sheep = "iq2_xs", env = {{ CONTEXT = "262144" }} }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
vram = "22000M"
ram = "37G"
idle = "2h"

[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
ram = "5G"
idle = "8h"
"#
    ))
}

#[tokio::test]
async fn a_running_sheep_serves_the_model_the_saved_state_names() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[("iq2_xs", "iq2_xs-256k"), ("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [counted(
            "iq2_xs-256k",
            Footprint {
                vram: Vram::Bytes(22_000 * MIB),
                ram: 37 * GIB,
            },
            false
        )]
    );
    assert!(discovered.stand_ins.is_empty());
    assert_eq!(http.seen().len(), 1, "the ready check is asked once");
}

#[tokio::test]
async fn a_sheep_whose_saved_model_a_lease_names_is_that_model_when_not_ready() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(503, "loading")])]);
    let config = shared_sheep(&base);
    let mut saved = saved_in(home.path(), &[("iq2_xs", "iq2_xs-256k")]);
    saved.leases.push(SavedLease {
        id: LeaseId(5),
        client: "bench-01".into(),
        model: "iq2_xs-256k".into(),
        priority: Priority::Batch,
        since: jiff::Timestamp::now(),
        expected_until: None,
        note: None,
        hold: SavedHold::Connection {},
        last_activity: None,
        release_if_idle_ms: None,
        reclaimable: false,
    });
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");

    let discovered = found(&config, shepherd, &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|found| &found.model).collect();
    assert_eq!(names, [&ModelName::from("iq2_xs-256k")]);
    assert!(discovered.stand_ins.is_empty());
}

#[tokio::test]
async fn a_ready_check_that_passes_on_a_later_try_counts() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let answers = vec![(503, "loading"), (200, r#"{"loaded":true}"#)];
    let (base, http) = fake_http(vec![("GET", "/health", answers)]);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(&config, shepherd, &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|found| &found.model).collect();
    assert_eq!(names, [&ModelName::from("laya")]);
    assert_eq!(http.seen().len(), 2);
}

#[tokio::test]
async fn a_running_sheep_with_no_record_is_unknown_at_its_largest_footprint() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [counted(
            "sheep:iq2_xs",
            Footprint {
                vram: Vram::Bytes(22_000 * MIB),
                ram: 44 * GIB,
            },
            true
        )]
    );
    assert_eq!(stand_ins(&discovered), ["sheep:iq2_xs"]);
    assert!(http.seen().is_empty(), "no model to ask a ready check of");
}

#[tokio::test]
async fn a_stand_in_name_never_takes_a_configured_models_name() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let config = config(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.iq2_xs]
backend = { sheep = "iq2_xs" }
url = "http://127.0.0.1:8080"
ram = "5G"
idle = "8h"

[models."sheep:iq2_xs"]
backend = { sheep = "iq2_xs" }
url = "http://127.0.0.1:8080"
ram = "6G"
idle = "8h"
"#,
    );
    let saved = saved_in(home.path(), &[]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");

    let discovered = found(&config, shepherd, &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|found| &found.model).collect();
    assert_eq!(names, [&ModelName::from("sheep:sheep:iq2_xs")]);
    assert!(!config.models.contains_key(names[0]));
    assert_eq!(stand_ins(&discovered), ["sheep:sheep:iq2_xs"]);
}

#[tokio::test]
async fn a_saved_model_gone_from_the_config_leaves_its_sheep_unknown() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[("iq2_xs", "iq2_xs-old"), ("laya", "iq2_xs")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");
    shepherd.running("laya");

    let discovered = found(&config, shepherd, &saved).await;

    // laya's sheep runs iq2_xs, which the config has since put on a sheep of its own.
    assert_eq!(stand_ins(&discovered), ["sheep:iq2_xs", "iq2_xs"]);
    let strays: Vec<_> = discovered.loaded.iter().map(|found| found.stray).collect();
    assert_eq!(strays, [false, false], "the dog loaded both sheep");
}

/// A sheep whose model is not ready still holds memory, so the sheep counts as unknown.
#[tokio::test]
async fn a_model_whose_ready_fails_is_not_counted() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[{"name":"qwen3.8:27b-ctx131072","size":26000000000,"size_vram":23000000000}]}"#;
    let (base, http) = fake_http(vec![
        ("GET", "/health", vec![(503, "loading")]),
        ("GET", "/api/ps", vec![(200, ps)]),
        ("GET", "/ready", vec![(200, r#"{"ok":false}"#)]),
    ]);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
ready = {{ path = "/ready", field = "ok" }}
vram = "22323M"
ram = "4G"
idle = "2h"

[models.iq3_s]
backend = {{ sheep = "iq3_s" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
vram = "all"
ram = "55G"
idle = "2h"
"#
    ));
    let saved = saved_in(home.path(), &[("iq3_s", "iq3_s")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq3_s");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [
            counted(
                "sheep:iq3_s",
                Footprint {
                    vram: Vram::All,
                    ram: 55 * GIB,
                },
                false
            ),
            counted(
                "ollama:qwen3.8:27b-ctx131072",
                Footprint {
                    vram: Vram::Bytes(23_000_000_000),
                    ram: 3_000_000_000,
                },
                true
            ),
        ]
    );
    assert_eq!(
        stand_ins(&discovered),
        ["sheep:iq3_s", "ollama:qwen3.8:27b-ctx131072"]
    );
    let readies = http
        .seen()
        .iter()
        .filter(|seen| seen.path != "/api/ps")
        .count();
    assert_eq!(
        readies, 6,
        "each ready check is tried three times, then given up"
    );
}

#[tokio::test]
async fn a_sheep_that_is_not_running_or_not_configured_is_left_out() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[("iq2_xs", "iq2_xs"), ("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.crash("iq2_xs");
    shepherd.running("web");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(discovered, Discovered::default());
}

#[tokio::test]
async fn a_saved_model_without_a_ready_check_is_loaded_once_its_sheep_runs() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let config = config(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#,
    );
    let saved = saved_in(home.path(), &[("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [counted(
            "laya",
            Footprint {
                vram: Vram::None,
                ram: 5 * GIB,
            },
            false
        )]
    );
}

/// The sheep crashed while the dog was down, and shep will start it again: its memory still counts.
#[tokio::test]
async fn a_sheep_waiting_to_restart_counts_as_unknown() {
    let home = tempfile::TempDir::new().expect("tempdir");
    // Bound, then dropped, so the ready check is refused as the crashed sheep would refuse it.
    let (base, http) = fake_http(Vec::new());
    drop(http);
    let config = shared_sheep(&base);
    let saved = saved_in(home.path(), &[("iq2_xs", "iq2_xs")]);
    let shepherd = FakeShepherd::new();
    shepherd.waiting_restart("iq2_xs");

    let discovered = found(&config, shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [counted(
            "sheep:iq2_xs",
            Footprint {
                vram: Vram::Bytes(22_000 * MIB),
                ram: 44 * GIB,
            },
            false
        )]
    );
    assert_eq!(stand_ins(&discovered), ["sheep:iq2_xs"]);
}

/// A ready check on a loopback socket that marks `asked` when a request arrives and answers
/// ready only once `other` is marked, so it answers only while the other check is asked too.
async fn paired_ready(
    asked: tokio::sync::watch::Sender<bool>,
    other: tokio::sync::watch::Receiver<bool>,
) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut other = other.clone();
            let asked = asked.clone();
            tokio::spawn(async move {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                asked.send_replace(true);
                if other.wait_for(|marked| *marked).await.is_err() {
                    return;
                }
                let body = r#"{"loaded":true}"#;
                let answer = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes()).await;
            });
        }
    });
    base
}

/// Each ready check answers only while the other is asked, so asking them one after another
/// gets no answer from the first.
#[tokio::test]
async fn ready_checks_at_start_run_together() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (first_asked, first_seen) = tokio::sync::watch::channel(false);
    let (second_asked, second_seen) = tokio::sync::watch::channel(false);
    let first = paired_ready(first_asked, second_seen).await;
    let second = paired_ready(second_asked, first_seen).await;
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.iq3_s]
backend = {{ sheep = "iq3_s" }}
url = "{first}"
ready = {{ path = "/health", field = "loaded" }}
ram = "5G"
idle = "2h"

[models.laya]
backend = {{ sheep = "laya" }}
url = "{second}"
ready = {{ path = "/health", field = "loaded" }}
ram = "5G"
idle = "8h"
"#
    ));
    let saved = saved_in(home.path(), &[("iq3_s", "iq3_s"), ("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq3_s");
    shepherd.running("laya");
    let backends = Backends::new(shepherd, crate::outbound::http_client());

    // Under one ready check's own timeout, so a first check left waiting fails here.
    let bound = Duration::from_secs(4);
    let discovered = timeout(bound, discover(&config, &backends, &saved))
        .await
        .unwrap_or_else(|_| panic!("discovery took longer than {bound:?}"));

    let names: Vec<_> = discovered.loaded.iter().map(|found| &found.model).collect();
    assert_eq!(names, [&ModelName::from("iq3_s"), &ModelName::from("laya")]);
}
