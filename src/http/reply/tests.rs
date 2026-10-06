//! The JSON replies and the sentences that say why a waiter waits.

use std::time::Duration;

use http_body_util::BodyExt;
use hyper::{StatusCode, body::Body as _, header::RETRY_AFTER};
use tokio::time::timeout;

use super::super::{Body, reply};
use crate::{
    book::{Reason, Refusal},
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
