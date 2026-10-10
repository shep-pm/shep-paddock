use super::*;
use crate::{
    book::Leased,
    footprint::{Footprint, Vram},
};

fn bare_view(clock: &Clock) -> LeaseView {
    let moment = clock.moment();
    LeaseView {
        id: LeaseId(12),
        client: ClientName::from("bench-01"),
        model: None,
        footprint: Some(Footprint {
            vram: Vram::Bytes(12 << 30),
            ram: 4 << 30,
        }),
        pid: Some(4_321),
        priority: Priority::Batch,
        since: Moment(moment.0 - 5_000),
        expected_until: None,
        note: Some("fine-tune run 3".to_owned()),
        hold: Hold::Connection,
        attached: true,
        reclaimable: false,
        last_activity: Moment(moment.0 - 5_000),
        in_use: false,
        release_if_idle: None,
        revoked: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_bare_lease_is_saved_with_its_footprint_and_pid() {
    let clock = Clock::new();
    let saved = SavedLease::from_view(bare_view(&clock), &clock);
    let written = serde_json::to_value(&saved).expect("json");
    assert_eq!(written["model"], json!(null));
    assert_eq!(
        written["footprint"],
        json!({ "vram": { "bytes": 12_884_901_888_u64 }, "ram_bytes": 4_294_967_296_u64 })
    );
    assert_eq!(written["pid"], json!(4_321));

    let restored = saved.restored(&clock).expect("a bare lease");
    assert_eq!(
        restored.ask.leased,
        Leased::Bare {
            footprint: Footprint {
                vram: Vram::Bytes(12 << 30),
                ram: 4 << 30
            },
            pid: Some(4_321)
        }
    );
}

#[test]
fn vram_is_written_as_none_all_or_bytes() {
    let written = |vram: Vram| {
        serde_json::to_value(SavedFootprint::from(Footprint { vram, ram: 0 })).expect("json")
            ["vram"]
            .clone()
    };
    assert_eq!(written(Vram::None), json!("none"));
    assert_eq!(written(Vram::All), json!("all"));
    assert_eq!(written(Vram::Bytes(7)), json!({ "bytes": 7 }));
    for vram in [Vram::None, Vram::All, Vram::Bytes(7)] {
        let back: SavedFootprint =
            serde_json::from_value(json!({ "vram": written(vram), "ram_bytes": 0 }))
                .expect("reads");
        assert_eq!(Footprint::from(back).vram, vram);
    }
}

#[tokio::test(start_paused = true)]
async fn a_lease_naming_neither_a_model_nor_a_footprint_is_not_restored() {
    let clock = Clock::new();
    let mut saved = SavedLease::from_view(bare_view(&clock), &clock);
    saved.footprint = None;
    assert_eq!(saved.restored(&clock), None);
}

/// The version 3 bytes of a bare lease as written to disk: they must keep reading as the same value.
#[test]
fn a_version_3_bare_lease_reads() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    let v3 = r#"{
  "version": 3,
  "leases": [
    {
      "id": 12,
      "client": "bench-01",
      "model": null,
      "priority": "batch",
      "since": "2026-10-10T08:00:00Z",
      "expected_until": null,
      "note": "fine-tune run 3",
      "hold": { "connection": {} },
      "last_activity": "2026-10-10T08:00:00Z",
      "release_if_idle_ms": null,
      "reclaimable": false,
      "footprint": { "vram": { "bytes": 12884901888 }, "ram_bytes": 4294967296 },
      "pid": 4321
    }
  ],
  "sheep": {},
  "models": {}
}"#;
    std::fs::write(&path, v3).expect("written");

    let saved = load(&path).expect("loaded").expect("a file");
    assert_eq!(saved.version, 3);
    let lease = &saved.leases[0];
    assert_eq!(lease.model, None);
    assert_eq!(
        lease.footprint,
        Some(SavedFootprint {
            vram: SavedVram::Bytes(12_884_901_888),
            ram_bytes: 4_294_967_296
        })
    );
    assert_eq!(lease.pid, Some(4_321));
}
