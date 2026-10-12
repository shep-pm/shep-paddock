use std::time::Duration;

use serde_json::json;

use super::{render, status};
use crate::{cli::Link, test_support::fake_http};

fn link(url: String) -> Link {
    Link {
        url,
        key: "k-bench".to_owned(),
        retry: Duration::from_millis(10),
        silence: Duration::from_secs(45),
    }
}

fn sample() -> serde_json::Value {
    json!({
        "host": { "vram_bytes": 25757220864u64, "ram_bytes": 66519769088u64,
                 "vram_declared_bytes": 5368709120u64, "ram_declared_bytes": 1536u64,
                 "unaccounted_vram_bytes": 1073741824u64 },
        "models": [
            { "model": "iq2_xs", "state": "loaded", "in_flight": 0, "last_used": "2026-10-04T10:00:00Z",
              "held_by": ["bench-01"], "unknown": false, "placement": null, "stray": false,
              "footprint": { "vram_bytes": 5368709120u64, "ram_bytes": 1536u64, "vram_all": false },
              "measured": { "vram_bytes": null, "ram_bytes": 80000000u64 },
              "measured_peak": { "vram_bytes": null, "ram_bytes": 90000000u64 },
              "turn_ram_bytes": 536870912u64, "turns": { "lease": 2, "request": 1 }, "drift": false },
            { "model": "laya", "state": "loaded", "in_flight": 0, "last_used": "2026-10-04T10:30:00Z",
              "held_by": [], "unknown": false, "placement": "ram", "stray": true,
              "footprint": { "vram_bytes": 25757220864u64, "ram_bytes": 2147483648u64, "vram_all": true },
              "measured": { "vram_bytes": 314572800u64, "ram_bytes": null },
              "measured_peak": { "vram_bytes": 524288000u64, "ram_bytes": null },
              "turn_ram_bytes": null, "turns": null, "drift": true },
            { "model": "qwen", "state": "unloaded", "in_flight": 2, "last_used": null,
              "held_by": [], "unknown": false, "placement": null, "stray": false,
              "footprint": { "vram_bytes": 0, "ram_bytes": 0, "vram_all": false },
              "measured": { "vram_bytes": null, "ram_bytes": null },
              "measured_peak": { "vram_bytes": null, "ram_bytes": null },
              "turn_ram_bytes": null, "turns": null, "drift": false }
        ],
        "leases": [
            { "id": "L1", "client": "bench-01", "model": "iq2_xs", "since": "2026-10-04T10:05:00Z",
              "expected_until": null, "note": "strata h2h run 3", "hold": "connection", "attached": true,
              "last_activity": "2026-10-04T10:20:00Z", "idle_for": 600, "release_if_idle": 1800,
              "reclaimable": false },
            { "id": "L2", "client": "mac-sessions", "model": "laya", "since": "2026-10-04T10:20:00Z",
              "expected_until": null, "note": null, "hold": "heartbeat", "attached": true,
              "last_activity": "2026-10-04T10:20:00Z", "idle_for": 0, "release_if_idle": null,
              "reclaimable": true }
        ],
        "waiters": [
            { "client": "mac-sessions", "model": "qwen", "kind": "request", "priority": "interactive",
              "since": "2026-10-04T11:00:00Z", "reason": "iq2_xs is held by bench-01", "estimate": null }
        ],
        "errors": [
            { "model": "qwen", "at": "2026-10-04T09:00:00Z", "error": "ollama answered 500" }
        ]
    })
}

#[test]
fn the_status_is_a_table_of_host_models_leases_waiters_and_errors() {
    let expected = [
        "host",
        "RESOURCE  TOTAL      DECLARED",
        "vram      23.99 GiB  5 GiB",
        "ram       61.95 GiB  1.5 KiB",
        "unaccounted VRAM: 1 GiB",
        "",
        "models",
        "MODEL   STATE     PLACEMENT  FOOTPRINT                MEASURED               PEAK                   TURNS                IN-FLIGHT  HELD-BY   LAST-USED             DRIFT",
        "iq2_xs  loaded    -          5 GiB VRAM, 1.5 KiB RAM  - VRAM, 76.29 MiB RAM  - VRAM, 85.83 MiB RAM  2 lease + 1 request  0          bench-01  2026-10-04T10:00:00Z  -",
        "laya    loaded    ram        all VRAM, 2 GiB RAM      300 MiB VRAM, - RAM    500 MiB VRAM, - RAM    -                    0          -         2026-10-04T10:30:00Z  yes",
        "qwen    unloaded  -          0 B VRAM, 0 B RAM        - VRAM, - RAM          - VRAM, - RAM          -                    2          -         -                     -",
        "",
        "leases",
        "ID  CLIENT        MODEL   HOLD        SINCE                 EXPECTED-UNTIL  IDLE  RECLAIMABLE  MEASURED  DRIFT  REVOKED-BY  NOTE",
        "L1  bench-01      iq2_xs  connection  2026-10-04T10:05:00Z  -               10m   -            -         -      -           strata h2h run 3",
        "L2  mac-sessions  laya    heartbeat   2026-10-04T10:20:00Z  -               0s    yes          -         -      -           -",
        "",
        "waiters",
        "CLIENT        MODEL  KIND     PRIORITY     SINCE                 REASON",
        "mac-sessions  qwen   request  interactive  2026-10-04T11:00:00Z  iq2_xs is held by bench-01",
        "",
        "errors",
        "MODEL  AT                    ERROR",
        "qwen   2026-10-04T09:00:00Z  ollama answered 500",
        "",
    ]
    .join("\n");
    assert_eq!(render(&sample()), expected);
}

#[test]
fn the_models_table_prints_all_for_vram_all() {
    let text = render(&sample());
    let laya = text
        .lines()
        .find(|line| line.starts_with("laya "))
        .expect("a laya row");
    assert!(laya.contains("all VRAM, 2 GiB RAM"), "{laya}");
    assert!(
        !laya.contains("23.99 GiB"),
        "all is not shown as the host's size: {laya}"
    );
}

/// The cells of `model`'s row in the models table, split at two or more spaces.
fn model_cells(text: &str, model: &str) -> Vec<String> {
    let row = text
        .lines()
        .find(|line| line.starts_with(&format!("{model} ")))
        .unwrap_or_else(|| panic!("a {model} row in {text}"));
    row.split("  ")
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_models_table_prints_measured_and_peak() {
    let text = render(&sample());

    let header = text
        .lines()
        .find(|line| line.starts_with("MODEL   STATE"))
        .expect("the models header");
    assert!(header.contains("MEASURED"), "{header}");
    assert!(header.contains("PEAK"), "{header}");
    let iq2 = model_cells(&text, "iq2_xs");
    assert!(iq2.contains(&"- VRAM, 76.29 MiB RAM".to_owned()), "{iq2:?}");
    assert!(iq2.contains(&"- VRAM, 85.83 MiB RAM".to_owned()), "{iq2:?}");
    let laya = model_cells(&text, "laya");
    assert!(laya.contains(&"300 MiB VRAM, - RAM".to_owned()), "{laya:?}");
    assert!(laya.contains(&"500 MiB VRAM, - RAM".to_owned()), "{laya:?}");
    let qwen = model_cells(&text, "qwen");
    assert_eq!(
        qwen.iter().filter(|cell| *cell == "- VRAM, - RAM").count(),
        2,
        "{qwen:?}"
    );
}

#[test]
fn the_models_table_prints_turns_as_lease_plus_request() {
    let text = render(&sample());

    assert!(model_cells(&text, "iq2_xs").contains(&"2 lease + 1 request".to_owned()));
    assert!(
        !model_cells(&text, "laya")
            .iter()
            .any(|cell| cell.contains("lease")),
        "a model without sequences has no turns"
    );
}

#[test]
fn control_characters_in_other_clients_text_are_escaped() {
    let mut hostile = sample();
    hostile["leases"][0]["note"] = json!("run\u{1b}[2J\u{1b}]0;owned\u{7}\nnext\u{9b}31m");
    hostile["leases"][0]["client"] = json!("evil\u{1b}[31m");
    hostile["models"][0]["held_by"] = json!(["bench\u{1b}[2J", "ok"]);
    let text = render(&hostile);
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "{text:?}"
    );
    assert_eq!(text.lines().count(), render(&sample()).lines().count());
    assert!(text.contains("run\\u{1b}[2J"), "{text:?}");
}

#[test]
fn an_empty_section_says_so() {
    let empty = json!({ "models": [], "leases": [], "waiters": [], "errors": [] });
    assert_eq!(
        render(&empty),
        "host\n(none)\n\nmodels\n(none)\n\nleases\n(none)\n\nwaiters\n(none)\n\nerrors\n(none)\n"
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

#[test]
fn sizes_use_binary_units_and_drop_a_whole_number_s_decimals() {
    let host = |vram: u64| {
        json!({ "host": { "vram_bytes": vram, "ram_bytes": 0,
                          "vram_declared_bytes": 0, "ram_declared_bytes": 0 } })
    };
    let first_row = |vram: u64| render(&host(vram)).lines().nth(2).unwrap().to_owned();
    assert!(first_row(1023).contains("1023 B"));
    assert!(first_row(1024).contains("1 KiB"));
    assert!(first_row(3 << 30).contains("3 GiB"));
    assert!(first_row(5 << 40).contains("5 TiB"));
}

/// The README's Status section, from its heading to the next.
fn readme_status() -> &'static str {
    let readme = include_str!("../../../README.md");
    let (_, from) = readme.split_once("## Status\n").expect("a Status section");
    from.split("\n## ").next().unwrap_or(from)
}

#[test]
fn the_readme_names_only_columns_the_status_prints() {
    let printed = render(&sample());
    let headers: Vec<&str> = printed
        .lines()
        .filter(|line| {
            line.split_whitespace()
                .all(|word| word.chars().all(|c| c.is_ascii_uppercase() || c == '-'))
        })
        .flat_map(str::split_whitespace)
        .collect();
    let named: Vec<&str> = readme_status()
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|code| code.len() > 1 && code.chars().all(|c| c.is_ascii_uppercase() || c == '-'))
        .collect();

    for column in &named {
        assert!(
            headers.contains(column),
            "the README names {column}, which the table lacks"
        );
    }
    for column in [
        "PLACEMENT",
        "FOOTPRINT",
        "PEAK",
        "TURNS",
        "DRIFT",
        "IDLE",
        "RECLAIMABLE",
        "MEASURED",
        "REVOKED-BY",
    ] {
        assert!(named.contains(&column), "the README leaves out {column}");
    }
}

#[test]
fn a_bare_lease_shows_its_footprint_measured_vram_and_who_revoked_it() {
    let status = json!({
        "leases": [
            { "id": "L3", "client": "bench-01", "model": null, "since": "2026-10-04T11:00:00Z",
              "expected_until": null, "note": "fine-tune run 3", "hold": "connection", "attached": true,
              "last_activity": "2026-10-04T11:00:00Z", "idle_for": 300, "release_if_idle": null,
              "reclaimable": false,
              "footprint": { "vram_bytes": 8_589_934_592_u64, "ram_bytes": 2_147_483_648_u64 },
              "measured": { "vram_bytes": 7_340_032_000_u64, "ram_bytes": null }, "drift": false,
              "revoked": { "by": "mac-sessions", "note": "forgotten" } }
        ]
    });
    let text = render(&status);
    let leases: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "leases")
        .skip(1)
        .take(2)
        .collect();
    assert_eq!(
        leases,
        [
            "ID  CLIENT    MODEL                  HOLD        SINCE                 EXPECTED-UNTIL  IDLE  RECLAIMABLE  MEASURED  DRIFT  REVOKED-BY    NOTE",
            "L3  bench-01  8 GiB VRAM, 2 GiB RAM  connection  2026-10-04T11:00:00Z  -               5m    -            6.84 GiB  -      mac-sessions  fine-tune run 3",
        ]
    );
}

#[test]
fn a_bare_lease_asking_for_all_vram_prints_all_in_its_model_cell() {
    let status = json!({
        "leases": [
            { "id": "L4", "client": "bench-01", "model": null, "since": "2026-10-04T11:00:00Z",
              "expected_until": null, "note": null, "hold": "connection", "attached": true,
              "last_activity": "2026-10-04T11:00:00Z", "idle_for": null, "release_if_idle": null,
              "reclaimable": false,
              "footprint": { "vram_bytes": 25_757_220_864_u64, "ram_bytes": 2_147_483_648_u64, "vram_all": true },
              "measured": null, "drift": false, "revoked": null }
        ]
    });
    let text = render(&status);
    let row = text
        .lines()
        .skip_while(|line| *line != "leases")
        .nth(2)
        .expect("a lease row");
    assert!(row.contains("all VRAM, 2 GiB RAM"), "{row}");
    assert!(!row.contains("23.99 GiB"), "{row}");
}
