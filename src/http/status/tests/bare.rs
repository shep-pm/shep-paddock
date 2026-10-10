//! A bare lease in the status: its footprint, measured VRAM, drift and revoke.

use super::*;
use crate::book::Revocation;

fn host() -> Host {
    Host {
        vram: 25_757_220_864,
        ram: 66_520_760_320,
    }
}

#[tokio::test(start_paused = true)]
async fn a_bare_lease_shows_its_footprint_measured_vram_drift_and_revoke() {
    let clock = clock();
    let mut snapshot = snapshot(&clock);
    let bare = LeaseView {
        id: LeaseId(3),
        model: None,
        footprint: Some(Footprint {
            vram: Vram::Bytes(8 << 30),
            ram: 2 << 30,
        }),
        pid: Some(4_321),
        measured: Measured {
            vram: Some(7_000 << 20),
            ram: None,
        },
        drift: false,
        revoked: Some(Revocation {
            by: ClientName::from("mac-sessions"),
            note: Some("forgotten".to_owned()),
        }),
        ..snapshot.leases[0].clone()
    };
    let all = LeaseView {
        id: LeaseId(4),
        footprint: Some(Footprint {
            vram: Vram::All,
            ram: 0,
        }),
        revoked: None,
        ..bare.clone()
    };
    let unmeasured = LeaseView {
        id: LeaseId(5),
        measured: Measured::default(),
        ..all.clone()
    };
    snapshot.leases = vec![bare, all, unmeasured];

    let body = status_body(&snapshot, &host(), &clock);
    let leases = &body["leases"];
    assert_eq!(leases[0]["model"], json!(null));
    assert_eq!(
        leases[0]["footprint"],
        json!({ "vram_bytes": 8_589_934_592_u64, "ram_bytes": 2_147_483_648_u64 })
    );
    assert_eq!(
        leases[0]["measured"],
        json!({ "vram_bytes": 7_340_032_000_u64, "ram_bytes": null })
    );
    assert_eq!(leases[0]["drift"], json!(false));
    assert_eq!(
        leases[0]["revoked"],
        json!({ "by": "mac-sessions", "note": "forgotten" })
    );
    assert_eq!(
        leases[1]["footprint"]["vram_bytes"],
        json!(25_757_220_864_u64),
        "all reads as the host's VRAM"
    );
    assert_eq!(leases[1]["revoked"], json!(null));
    assert_eq!(
        leases[2]["measured"],
        json!(null),
        "a bare lease no GPU process descends from is unmeasured"
    );
}

#[tokio::test(start_paused = true)]
async fn a_model_lease_has_no_footprint_or_measurement() {
    let clock = clock();
    let body = status_body(&snapshot(&clock), &host(), &clock);
    assert_eq!(body["leases"][0]["footprint"], json!(null));
    assert_eq!(body["leases"][0]["measured"], json!(null));
    assert_eq!(body["leases"][0]["drift"], json!(false));
}
