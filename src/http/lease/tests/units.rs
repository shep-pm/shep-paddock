//! Pure functions of the lease routes.

use std::time::Duration;

use super::super::{Take, parse_id, render_id, stream};
use crate::{
    book::{Ended, LeaseId, Revocation},
    config::ClientName,
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
        assert!(take.request().is_ok(), "{body}");
    }
}
