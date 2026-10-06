//! The engine starting from a real discovery of a fake shepherd and a fake HTTP server.
//!
//! The fake server is a loopback socket, so these tests run on real time.

use super::{
    restart::{bench_lease, saved_with},
    *,
};
use crate::{
    book::Refusal,
    discover::discover,
    saved::{Saved, SavedHold, SavedLease},
};

/// iq2_xs is not ready when the dog restarts, but a lease names it, so it is
/// restored as iq2_xs and the lease keeps it from a request that needs its room.
#[tokio::test]
async fn a_leased_sheep_not_ready_at_a_restart_stays_held() {
    let (base, _health) = fake_http(vec![("GET", "/health", vec![(503, "loading")])]);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"

[[clients]]
name = "bench-01"
key = "k-bench"

[models.iq2_xs]
backend = {{ sheep = "iq2_xs" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
vram = "all"
ram = "37G"
idle = "2h"

[models.iq3_s]
backend = {{ sheep = "iq3_s" }}
url = "http://127.0.0.1:8081"
vram = "all"
ram = "55G"
idle = "2h"
"#
    ));
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");
    let lease = SavedLease {
        expected_until: None,
        ..bench_lease(5, "iq2_xs", SavedHold::Connection {})
    };
    let saved = saved_with(&[("iq2_xs", "iq2_xs")], vec![lease]);
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(SOON * 10, discover(&config, &backends, &saved))
        .await
        .expect("discovery finishes");
    let start = Start {
        saved,
        discovered,
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        let admitted = timeout(SOON * 10, admit(engine.clone(), "iq3_s"))
            .await
            .expect("answered");
        let Admission::Refused(Refusal {
            reason: Reason::Held { model, .. },
            ..
        }) = &admitted
        else {
            panic!("iq3_s was not refused as held: {admitted:?}");
        };
        assert_eq!(*model, ModelName::from("iq2_xs"));
        assert_eq!(state_of(&engine, "iq2_xs").await, Some(State::Loaded));
        let calls = shepherd.calls();
        assert!(
            !calls.iter().any(|call| matches!(call, Call::Stop(_))),
            "{calls:?}"
        );
    })
    .await;
}

/// What ollama holds for a model nobody configured counts, so a load that would
/// overcommit the card waits for it to be unloaded. Real time: the fake ollama is a socket.
#[tokio::test]
async fn an_unknown_ollama_model_is_unloaded_before_a_load_that_needs_its_room() {
    let ps = r#"{"models":[{"name":"llama3:8b","size":6000000000,"size_vram":5000000000}]}"#;
    let (base, ollama) = fake_http(vec![
        ("GET", "/api/ps", vec![(200, ps)]),
        ("POST", "/api/generate", vec![(200, "{}")]),
    ]);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ));
    let shepherd = FakeShepherd::new();
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(SOON * 10, discover(&config, &backends, &Saved::default()))
        .await
        .expect("discovery finishes");
    let start = Start {
        discovered,
        ..Start::default()
    };
    with_engine_from(config, shepherd, start, |engine| async move {
        let unknown = ModelName::from("ollama:llama3:8b");
        let snapshot = engine.snapshot().await;
        assert!(
            snapshot
                .models
                .iter()
                .any(|view| view.name == unknown && view.unknown && view.state == State::Loaded),
            "{snapshot:?}"
        );

        let in_flight = timeout(SOON * 10, admit(engine.clone(), "qwen3.8:27b"))
            .await
            .expect("answered");
        assert!(matches!(in_flight, Admission::Forward(_)), "{in_flight:?}");

        let posted: Vec<serde_json::Value> = ollama
            .seen()
            .iter()
            .filter(|seen| seen.path == "/api/generate")
            .map(|seen| serde_json::from_str(&seen.body).expect("JSON"))
            .collect();
        assert_eq!(
            posted,
            [
                serde_json::json!({ "model": "llama3:8b", "keep_alive": 0 }),
                serde_json::json!({ "model": "qwen3.8:27b-ctx131072", "keep_alive": -1 }),
            ]
        );
        assert_eq!(state_of(&engine, "ollama:llama3:8b").await, None);
    })
    .await;
}

/// An ollama silent at a restart may hold its models still, so the status says it could not ask.
#[tokio::test]
async fn an_ollama_silent_at_a_restart_is_an_error_and_its_models_unloaded() {
    // Bound, then dropped, so the port refuses the connection.
    let (base, http) = fake_http(Vec::new());
    drop(http);
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
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ));
    let shepherd = FakeShepherd::new();
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(SOON * 10, discover(&config, &backends, &Saved::default()))
        .await
        .expect("discovery finishes");
    let start = Start {
        discovered,
        ..Start::default()
    };
    with_engine_from(config, shepherd, start, |engine| async move {
        let snapshot = timeout(SOON * 10, engine.snapshot())
            .await
            .expect("answered");
        let qwen = ModelName::from("qwen3.8:27b");
        let state = snapshot.models.iter().find(|view| view.name == qwen);
        assert_eq!(state.map(|view| view.state), Some(State::Unloaded));
        let [error] = snapshot.errors.as_slice() else {
            panic!("not one error: {:?}", snapshot.errors);
        };
        assert_eq!(error.model, qwen);
        assert!(
            error
                .error
                .starts_with("backend ollama did not answer at start: "),
            "{}",
            error.error
        );
    })
    .await;
}
