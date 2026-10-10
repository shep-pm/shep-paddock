//! Pure functions of the lease routes.

use std::time::Duration;

use super::super::{Take, parse_id, render_id, stream};
use crate::{
    book::{Ended, LeaseId, Leased, Revocation},
    config::{ClientName, ModelName},
    footprint::{Footprint, Vram},
    http::Peer,
};

#[test]
fn lease_ids_render_as_l_and_parse_back_strictly() {
    assert_eq!(render_id(LeaseId(7)), "L7");
    assert_eq!(parse_id("L7"), Some(LeaseId(7)));
    for bad in [
        "",
        "L",
        "7",
        "l7",
        "L+7",
        "L-7",
        "L07",
        "L7x",
        " L7",
        "L7 ",
        "L99999999999999999999",
    ] {
        assert_eq!(parse_id(bad), None, "{bad:?}");
    }
}

#[test]
fn each_ending_is_one_line_naming_its_reason() {
    for (why, reason) in [
        (Ended::Released, "released"),
        (Ended::Expired, "expired"),
        (Ended::Abandoned, "abandoned"),
        (Ended::Reclaimed, "reclaimed"),
    ] {
        assert_eq!(
            stream::ended_line(&why),
            serde_json::json!({ "ended": { "reason": reason } })
        );
    }
    assert_eq!(
        stream::ended_line(&Ended::Idle {
            after: Duration::from_secs(1_800)
        }),
        serde_json::json!({ "ended": { "reason": "idle", "idle_for": "30m" } })
    );
}

#[test]
fn a_revoked_line_names_who_and_why() {
    let by = |note: Option<&str>| {
        Ended::Revoked(Revocation {
            by: ClientName::from("mac-sessions"),
            note: note.map(str::to_owned),
        })
    };
    assert_eq!(
        stream::ended_line(&by(Some("forgotten since Tuesday"))),
        serde_json::json!({ "ended": { "reason": "revoked", "by": "mac-sessions", "note": "forgotten since Tuesday" } })
    );
    assert_eq!(
        stream::ended_line(&by(None)),
        serde_json::json!({ "ended": { "reason": "revoked", "by": "mac-sessions", "note": null } })
    );
}

/// A connection-held lease has no ttl, so one it sends is ignored rather than capped.
#[test]
fn a_long_ttl_on_a_connection_lease_is_not_refused() {
    for body in [
        r#"{"model":"iq2_xs","ttl":"2h"}"#,
        r#"{"model":"iq2_xs","hold":"connection","ttl":"2h"}"#,
    ] {
        let take = Take::parse(body.as_bytes()).expect("a take");
        assert!(take.request(false).is_ok(), "{body}");
    }
}

fn leased(body: &str, loopback: bool) -> Leased {
    let take = Take::parse(body.as_bytes()).expect("a take");
    take.request(loopback).expect("a request").0.leased
}

#[test]
fn a_bare_leases_pid_is_kept_only_over_loopback() {
    let body = r#"{"footprint":{"vram":"8G"},"pid":4321}"#;
    let pid = |loopback| match leased(body, loopback) {
        Leased::Bare { pid, .. } => pid,
        Leased::Model(_) => panic!("a bare lease"),
    };
    assert_eq!(pid(true), Some(4_321));
    assert_eq!(pid(false), None);
}

#[test]
fn a_model_leases_pid_is_ignored() {
    assert_eq!(
        leased(r#"{"model":"iq2_xs","pid":4321}"#, true),
        Leased::Model(ModelName::from("iq2_xs"))
    );
}

#[test]
fn a_footprint_reads_shep_sizes_and_all() {
    let footprint = |body: &str| match leased(body, false) {
        Leased::Bare { footprint, .. } => footprint,
        Leased::Model(_) => panic!("a bare lease"),
    };
    assert_eq!(
        footprint(r#"{"footprint":{"vram":"all","ram":"4G"}}"#),
        Footprint {
            vram: Vram::All,
            ram: 4 << 30
        }
    );
    assert_eq!(
        footprint(r#"{"footprint":{"ram":"512M"}}"#),
        Footprint {
            vram: Vram::None,
            ram: 512 << 20
        }
    );
    assert_eq!(
        footprint(r#"{"footprint":{"vram":"0"}}"#),
        Footprint {
            vram: Vram::Bytes(0),
            ram: 0
        },
        "a size of 0 counts as given"
    );
}

#[test]
fn loopback_is_127_8_and_1_and_their_mapped_forms() {
    for (addr, loopback) in [
        ("127.0.0.1:5000", true),
        ("127.9.9.9:5000", true),
        ("[::1]:5000", true),
        ("[::ffff:127.0.0.1]:5000", true),
        ("192.0.2.7:5000", false),
        ("[2001:db8::1]:5000", false),
        ("[::ffff:192.0.2.7]:5000", false),
    ] {
        let peer = Peer(addr.parse().expect("an address"));
        assert_eq!(peer.is_loopback(), loopback, "{addr}");
    }
}
