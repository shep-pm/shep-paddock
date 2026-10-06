//! Pure functions of the lease routes.

use super::super::{Take, parse_id, render_id, stream};
use crate::book::{Ended, LeaseId};

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
fn an_ended_lease_says_why() {
    for (why, text) in [
        (Ended::Released, "released"),
        (Ended::Expired, "expired"),
        (Ended::Abandoned, "abandoned"),
    ] {
        assert_eq!(stream::ended_text(why), text);
    }
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
