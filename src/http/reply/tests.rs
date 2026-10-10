//! The JSON replies and the sentences that say why a waiter waits.

use std::time::Duration;

use http_body_util::BodyExt;
use hyper::{StatusCode, body::Body as _, header::RETRY_AFTER};
use tokio::time::timeout;

use super::super::{Body, reply};
use crate::{
    book::{Reason, Refusal, TurnHolder},
    config::{ClientName, ModelName},
    engine::Clock,
};

// Past collecting a body that is already whole.
const LIMIT: Duration = Duration::from_secs(5);

async fn body_of(response: hyper::Response<Body>) -> serde_json::Value {
    assert!(response.body().size_hint().exact().is_some());
    let bytes = timeout(LIMIT, response.into_body().collect())
        .await
        .expect("the body in time")
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}

fn held(clock: &Clock, until: Option<&str>) -> Reason {
    let at = |text: &str| clock.moment_of(text.parse().expect("timestamp"));
    Reason::Held {
        model: "iq2_xs".into(),
        client: ClientName::from("bench-01"),
        lease: crate::book::LeaseId(1),
        since: at("2026-10-04T08:00:00Z"),
        until: until.map(at),
        idle_since: None,
    }
}

#[tokio::test]
async fn busy_has_retry_after_when_there_is_an_estimate() {
    let clock = Clock::new();
    let refusal = Refusal {
        reason: held(&clock, Some("2026-10-04T20:00:00Z")),
        retry_after: Some(Duration::from_millis(1500)),
    };

    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[RETRY_AFTER], "2");
    assert_eq!(
        body_of(response).await,
        serde_json::json!({
            "error": "busy",
            "model": "qwen3.8:27b",
            "reason": "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z",
            "reason_kind": "held",
            "expected_until": "2026-10-04T20:00:00Z",
        })
    );
}

#[tokio::test]
async fn busy_has_no_retry_after_without_one() {
    let clock = Clock::new();
    let refusal = Refusal {
        reason: held(&clock, None),
        retry_after: None,
    };

    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get(RETRY_AFTER).is_none());
    assert_eq!(
        body_of(response).await["expected_until"],
        serde_json::Value::Null
    );
}

/// `expected_until` for a refusal that is not a hold is the moment of the reply plus its retry.
async fn expected_until_of(reason: Reason, retry_after: Option<Duration>) -> serde_json::Value {
    let clock = Clock::new();
    let refusal = Refusal {
        reason,
        retry_after,
    };
    let before = clock.wall(clock.moment());
    let response = reply::busy(&ModelName::from("qwen3.8:27b"), &refusal, &clock);
    let after = clock.wall(clock.moment());
    let expected = body_of(response).await["expected_until"].clone();
    if let Some(retry) = retry_after {
        let at: jiff::Timestamp = expected
            .as_str()
            .expect("a string")
            .parse()
            .expect("rfc 3339");
        let retry = jiff::SignedDuration::try_from(retry).expect("duration");
        assert!(at >= before.checked_add(retry).expect("add"), "{at}");
        assert!(at <= after.checked_add(retry).expect("add"), "{at}");
    }
    expected
}

#[tokio::test]
async fn a_grace_refusal_expects_the_retry_after_from_now() {
    let clock = Clock::new();
    let reason = Reason::Grace {
        model: "iq2_xs".into(),
        until: clock.moment(),
    };
    expected_until_of(reason.clone(), Some(Duration::from_secs(90))).await;
    assert_eq!(
        expected_until_of(reason, None).await,
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn a_loading_refusal_expects_the_retry_after_from_now() {
    let reason = Reason::Loading {
        model: "iq2_xs".into(),
    };
    expected_until_of(reason.clone(), Some(Duration::from_secs(90))).await;
    assert_eq!(
        expected_until_of(reason, None).await,
        serde_json::Value::Null
    );
}

#[test]
fn each_reason_reads_as_a_sentence() {
    let clock = Clock::new();
    let at = clock.moment_of("2026-10-04T09:30:00Z".parse().expect("timestamp"));
    let model = |name: &str| ModelName::from(name);
    let say = |reason| reply::sentence(&reason, &clock);

    assert_eq!(
        say(Reason::Loading {
            model: model("iq2_xs")
        }),
        "iq2_xs is loading"
    );
    assert_eq!(
        say(Reason::Evicting {
            model: model("laya"),
            for_model: model("iq3_s")
        }),
        "laya is evicting for iq3_s"
    );
    assert_eq!(
        say(Reason::Draining {
            model: model("laya")
        }),
        "laya is unloading"
    );
    assert_eq!(
        say(Reason::Grace {
            model: model("iq2_xs"),
            until: at
        }),
        "iq2_xs is in its grace period until 2026-10-04T09:30:00Z"
    );
    assert_eq!(
        say(held(&clock, None)),
        "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z"
    );
    assert_eq!(
        say(Reason::Behind {
            model: model("iq3_s")
        }),
        "iq3_s is loading or claimed by another waiter"
    );
}

#[test]
fn a_held_reason_says_how_long_its_lease_has_been_idle() {
    let clock = Clock::started_at("2026-10-04T11:12:00Z".parse().expect("timestamp"));
    let at = |text: &str| clock.moment_of(text.parse().expect("timestamp"));
    let held = |idle_since: Option<&str>| Reason::Held {
        model: "iq2_xs".into(),
        client: ClientName::from("bench-01"),
        lease: crate::book::LeaseId(1),
        since: at("2026-10-04T08:00:00Z"),
        until: None,
        idle_since: idle_since.map(at),
    };
    let say = |reason: Reason| reply::sentence(&reason, &clock);
    assert_eq!(
        say(held(Some("2026-10-04T08:00:00Z"))),
        "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z, idle for 3h"
    );
    assert_eq!(
        say(held(Some("2026-10-04T11:00:00Z"))),
        "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z, idle for 12m"
    );
    assert_eq!(
        say(held(None)),
        "iq2_xs is held by bench-01 since 2026-10-04T08:00:00Z"
    );
}

/// The idle time is read off the clock as the sentence is written, so each line states a fact
/// about its own moment.
#[tokio::test(start_paused = true)]
async fn a_held_reasons_idle_time_is_as_of_the_sentence() {
    let clock = Clock::started_at("2026-10-04T11:12:00Z".parse().expect("timestamp"));
    let reason = Reason::Held {
        model: "iq2_xs".into(),
        client: ClientName::from("bench-01"),
        lease: crate::book::LeaseId(1),
        since: clock.moment_of("2026-10-04T08:00:00Z".parse().expect("timestamp")),
        until: None,
        idle_since: Some(clock.moment_of("2026-10-04T09:00:00Z".parse().expect("timestamp"))),
    };
    let first = reply::sentence(&reason, &clock);
    tokio::time::advance(Duration::from_secs(3_600)).await;
    let later = reply::sentence(&reason, &clock);

    assert!(first.ends_with(", idle for 2h"), "{first}");
    assert!(later.ends_with(", idle for 3h"), "{later}");
}

#[test]
fn rough_rounds_down_to_its_largest_whole_unit() {
    for (seconds, text) in [
        (0, "0s"),
        (59, "59s"),
        (60, "1m"),
        (3_599, "59m"),
        (3_600, "1h"),
        (11_520, "3h"),
    ] {
        assert_eq!(
            reply::rough(Duration::from_secs(seconds)),
            text,
            "{seconds}"
        );
    }
}

#[test]
fn a_turn_reason_names_each_holder_with_its_note_and_the_waiters_place() {
    let clock = Clock::new();
    let holder = |client: &str, note: Option<&str>| TurnHolder {
        client: ClientName::from(client),
        note: note.map(str::to_owned),
        until: None,
    };
    let turn = |holders, ahead| Reason::Turn {
        model: ModelName::from("iq3_s"),
        holders,
        ahead,
    };

    let first = turn(vec![holder("kelpie", Some("#79"))], 0);
    let later = turn(
        vec![holder("kelpie", Some("a\nb")), holder("bench-01", None)],
        2,
    );

    assert_eq!(
        reply::sentence(&first, &clock),
        r##"iq3_s is serving kelpie "#79", next in line"##
    );
    assert_eq!(
        reply::sentence(&later, &clock),
        r#"iq3_s is serving kelpie "a\nb" and bench-01, 2 ahead"#
    );
    assert_eq!(reply::kind(&first), "turn");
}

#[test]
fn every_reason_kind_keeps_its_word() {
    let model = || ModelName::from("iq3_s");
    let at = crate::book::Moment(0);
    let reasons = [
        (Reason::Loading { model: model() }, "loading"),
        (
            Reason::Evicting {
                model: model(),
                for_model: model(),
            },
            "evicting",
        ),
        (Reason::Draining { model: model() }, "draining"),
        (
            Reason::Grace {
                model: model(),
                until: at,
            },
            "grace",
        ),
        (held(&Clock::new(), None), "held"),
        (Reason::Behind { model: model() }, "behind"),
        (
            Reason::Turn {
                model: model(),
                holders: Vec::new(),
                ahead: 0,
            },
            "turn",
        ),
    ];

    for (reason, word) in reasons {
        assert_eq!(reply::kind(&reason), word, "{reason:?}");
    }
}
