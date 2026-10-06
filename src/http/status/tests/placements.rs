//! Slice 2's status fields.

use super::*;
use crate::config::PlacementName;

const MIB: u64 = 1 << 20;

fn host() -> Host {
    Host {
        vram: 25_757_220_864,
        ram: 66_520_760_320,
    }
}

/// laya in RAM as a drifting stray, and bench-01's reclaimable lease on it, last used at 09:50.
fn placed_snapshot(clock: &Clock, in_use: bool, unaccounted: Option<u64>) -> Snapshot {
    let at = |text: &str| clock.moment_of(text.parse().expect("a timestamp"));
    Snapshot {
        models: vec![ModelView {
            placement: Some(PlacementName::from("ram")),
            stray: true,
            measured: Measured {
                vram: Some(300 * MIB),
                ram: None,
            },
            drift: true,
            ..view("laya", State::Loaded, at("2026-10-04T09:50:00Z"))
        }],
        leases: vec![LeaseView {
            id: LeaseId(3),
            client: ClientName::from("bench-01"),
            model: ModelName::from("laya"),
            priority: Priority::Batch,
            since: at("2026-10-04T08:00:00Z"),
            expected_until: None,
            note: Some("step 412/900".to_owned()),
            hold: Hold::Connection,
            attached: true,
            reclaimable: true,
            last_activity: at("2026-10-04T09:50:00Z"),
            in_use,
            release_if_idle: Some(Duration::from_secs(1_800)),
        }],
        waiters: vec![],
        errors: vec![],
        declared: Footprint {
            vram: Vram::Bytes(0),
            ram: 5 << 30,
        },
        unaccounted_vram: unaccounted,
    }
}

#[test]
fn the_status_shows_placements_strays_drift_and_idle_leases() {
    let clock = clock();
    let body = status_body(
        &placed_snapshot(&clock, false, Some(2 << 30)),
        &host(),
        &clock,
    );
    assert_eq!(
        body["host"]["unaccounted_vram_bytes"],
        json!(2_147_483_648_u64)
    );
    assert_eq!(
        body["models"][0],
        json!({
            "model": "laya", "state": "loaded", "in_flight": 0,
            "last_used": "2026-10-04T09:50:00Z", "held_by": [], "unknown": false,
            "placement": "ram", "stray": true,
            "measured": { "vram_bytes": 314_572_800_u64, "ram_bytes": null },
            "drift": true,
        })
    );
    assert_eq!(
        body["leases"][0],
        json!({
            "id": "L3", "client": "bench-01", "model": "laya",
            "since": "2026-10-04T08:00:00Z", "expected_until": null,
            "note": "step 412/900", "hold": "connection", "attached": true,
            "last_activity": "2026-10-04T09:50:00Z", "idle_for": 600,
            "release_if_idle": 1800, "reclaimable": true,
        })
    );
}

#[test]
fn a_lease_in_use_is_idle_for_nothing_and_unaccounted_without_a_figure_is_absent() {
    let clock = clock();
    let body = status_body(&placed_snapshot(&clock, true, None), &host(), &clock);
    assert_eq!(body["leases"][0]["idle_for"], json!(0));
    assert!(
        body["host"].get("unaccounted_vram_bytes").is_none(),
        "{body}"
    );
}

#[test]
fn an_unloaded_model_has_no_placement_and_nothing_measured() {
    let clock = clock();
    let body = status_body(&snapshot(&clock), &host(), &clock);
    assert_eq!(body["models"][1]["model"], json!("laya"));
    assert_eq!(body["models"][1]["placement"], json!(null));
    assert_eq!(
        body["models"][1]["measured"],
        json!({ "vram_bytes": null, "ram_bytes": null })
    );
    assert_eq!(body["models"][1]["drift"], json!(false));
}

/// A lease's idle time is read off the clock as the status is written.
#[tokio::test(start_paused = true)]
async fn a_leases_idle_time_is_as_of_the_status() {
    let clock = clock();
    let snapshot = placed_snapshot(&clock, false, None);
    let first = status_body(&snapshot, &host(), &clock);
    tokio::time::advance(Duration::from_secs(60)).await;
    let later = status_body(&snapshot, &host(), &clock);

    assert_eq!(first["leases"][0]["idle_for"], json!(600));
    assert_eq!(later["leases"][0]["idle_for"], json!(660));
}

#[test]
fn a_release_if_idle_below_a_whole_second_rounds_up_and_never_reads_0() {
    let clock = clock();
    for (millis, seconds) in [(500, 1), (1_000, 1), (1_500, 2)] {
        let mut snapshot = placed_snapshot(&clock, false, None);
        snapshot.leases[0].release_if_idle = Some(Duration::from_millis(millis));
        let body = status_body(&snapshot, &host(), &clock);
        assert_eq!(
            body["leases"][0]["release_if_idle"],
            json!(seconds),
            "{millis}ms"
        );
    }
}
