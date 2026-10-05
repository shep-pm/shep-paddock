//! Discovery against the fake shepherd and a fake HTTP server on a real loopback socket, so these
//! tests run on real time. Every await is bounded by `LIMIT`.

use std::{path::Path, sync::Arc, time::Duration};

use tokio::time::timeout;

use super::*;
use crate::{
    footprint::Vram,
    saved,
    test_support::{FakeShepherd, config, fake_http},
};

mod ollama;

// Past one listing and a ready check or two on loopback.
const LIMIT: Duration = Duration::from_secs(10);

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// The saved state as a restart reads it: written to a scratch `$SHEP_HOME`, then loaded.
fn saved_in(home: &Path, sheep: &[(&str, &str)]) -> Saved {
    let path = saved::path_in(home);
    let written = Saved {
        sheep: sheep
            .iter()
            .map(|(sheep, model)| ((*sheep).to_owned(), ModelName::from(*model)))
            .collect(),
        ..Saved::default()
    };
    saved::store(&path, &written).expect("stored");
    saved::load(&path).expect("loaded").expect("present")
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
        (discovered.loaded, discovered.unknown),
        (
            vec![(
                ModelName::from("iq2_xs-256k"),
                Footprint {
                    vram: Vram::Bytes(22_000 * MIB),
                    ram: 37 * GIB,
                },
            )],
            Vec::<String>::new(),
        )
    );
    assert_eq!(http.seen().len(), 1, "the ready check is asked once");
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
        (discovered.loaded, discovered.unknown),
        (
            vec![(
                ModelName::from("sheep:iq2_xs"),
                Footprint {
                    vram: Vram::Bytes(22_000 * MIB),
                    ram: 44 * GIB,
                },
            )],
            vec!["iq2_xs".to_owned()],
        )
    );
    assert!(http.seen().is_empty(), "no model to ask a ready check of");
}

#[tokio::test]
async fn an_unknown_sheep_is_never_named_as_a_configured_model() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let config = config(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models."sheep:laya"]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#,
    );
    let saved = saved_in(home.path(), &[]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(&config, shepherd, &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|(name, _)| name).collect();
    assert_eq!(names, [&ModelName::from("sheep:sheep:laya")]);
    assert!(!config.models.contains_key(names[0]));
    assert_eq!(discovered.unknown, ["laya"]);
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

    assert_eq!(discovered.unknown, ["iq2_xs", "laya"]);
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
            (
                ModelName::from("sheep:iq3_s"),
                Footprint {
                    vram: Vram::All,
                    ram: 55 * GIB,
                },
            ),
            (
                ModelName::from("ollama:qwen3.8:27b-ctx131072"),
                Footprint {
                    vram: Vram::Bytes(23_000_000_000),
                    ram: 3_000_000_000,
                },
            ),
        ]
    );
    assert_eq!(discovered.unknown, ["iq3_s"]);
    let readies = http
        .seen()
        .iter()
        .filter(|seen| seen.path != "/api/ps")
        .count();
    assert_eq!(readies, 2, "each ready check is asked once, not polled");
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
        [(
            ModelName::from("laya"),
            Footprint {
                vram: Vram::None,
                ram: 5 * GIB,
            }
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
        [(
            ModelName::from("sheep:iq2_xs"),
            Footprint {
                vram: Vram::Bytes(22_000 * MIB),
                ram: 44 * GIB,
            },
        )]
    );
    assert_eq!(discovered.unknown, ["iq2_xs"]);
}
