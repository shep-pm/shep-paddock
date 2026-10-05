use std::time::Duration;

use serde_json::json;

use super::{render, status};
use crate::{cli::Link, test_support::fake_http};

fn link(url: String) -> Link {
    Link {
        url,
        key: "k-bench".to_owned(),
        retry: Duration::from_millis(10),
    }
}

fn sample() -> serde_json::Value {
    json!({
        "host": { "vram_bytes": 1, "ram_bytes": 1, "vram_declared_bytes": 1, "ram_declared_bytes": 1 },
        "models": [
            { "model": "iq2_xs", "state": "loaded", "in_flight": 0,
              "last_used": "2026-10-04T10:00:00Z", "held_by": ["bench-01"], "unknown": false },
            { "model": "qwen", "state": "unloaded", "in_flight": 2,
              "last_used": null, "held_by": [], "unknown": false }
        ],
        "leases": [
            { "id": "L1", "client": "bench-01", "model": "iq2_xs", "since": "2026-10-04T10:05:00Z",
              "expected_until": null, "note": "strata h2h run 3", "hold": "connection", "attached": true }
        ],
        "waiters": [
            { "client": "mac-sessions", "model": "qwen", "kind": "request", "priority": "interactive",
              "since": "2026-10-04T11:00:00Z", "reason": "iq2_xs is held by bench-01", "estimate": null }
        ],
        "errors": []
    })
}

#[test]
fn the_status_is_a_table_of_models_leases_and_waiters() {
    let expected = [
        "models",
        "MODEL   STATE     IN-FLIGHT  HELD-BY   LAST-USED",
        "iq2_xs  loaded    0          bench-01  2026-10-04T10:00:00Z",
        "qwen    unloaded  2          -         -",
        "",
        "leases",
        "ID  CLIENT    MODEL   HOLD        SINCE                 EXPECTED-UNTIL  NOTE",
        "L1  bench-01  iq2_xs  connection  2026-10-04T10:05:00Z  -               strata h2h run 3",
        "",
        "waiters",
        "CLIENT        MODEL  KIND     PRIORITY     SINCE                 REASON",
        "mac-sessions  qwen   request  interactive  2026-10-04T11:00:00Z  iq2_xs is held by bench-01",
        "",
    ]
    .join("\n");
    assert_eq!(render(&sample()), expected);
}

#[test]
fn an_empty_section_says_so() {
    let empty = json!({ "models": [], "leases": [], "waiters": [] });
    assert_eq!(
        render(&empty),
        "models\n(none)\n\nleases\n(none)\n\nwaiters\n(none)\n"
    );
}

#[tokio::test]
async fn status_prints_the_table_and_sends_the_key() {
    let body = Box::leak(sample().to_string().into_boxed_str());
    let (url, server) = fake_http(vec![("GET", "/paddock/status", vec![(200, body)])]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = status(&link(url), &mut out, &mut err).await;
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert_eq!(String::from_utf8_lossy(&out), render(&sample()));
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer k-bench"));
}

#[tokio::test]
async fn a_refused_key_exits_1_with_the_status_and_no_key() {
    let (url, _server) = fake_http(vec![(
        "GET",
        "/paddock/status",
        vec![(401, r#"{"error":"unauthorized"}"#)],
    )]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = status(&link(url), &mut out, &mut err).await;
    assert_eq!(code, 1);
    let said = String::from_utf8_lossy(&err);
    assert!(said.contains("401"), "{said}");
    assert!(!said.contains("k-bench"), "{said}");
    assert!(out.is_empty());
}

#[tokio::test]
async fn an_unreachable_dog_exits_1() {
    // Nothing listens on port 1.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = status(&link("http://127.0.0.1:1".to_owned()), &mut out, &mut err).await;
    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&err).contains("127.0.0.1:1"));
}
