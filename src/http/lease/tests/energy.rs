//! The energy a released lease says it used, over a real loopback socket on real time.

use super::*;
use crate::{
    book::Priority,
    engine::{Admission, Metered},
    meter::{Reading, Source},
};

/// A heartbeat lease on iq2_xs held by mac-sessions, with a request of its holder's in flight
async fn in_use(paddock: &Paddock) -> (String, crate::engine::InFlight) {
    let (_, body) = json_of(
        paddock
            .take("k-mac", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
            .await,
    )
    .await;
    let id = body["id"].as_str().expect("an id").to_owned();
    let admitted = paddock
        .engine
        .admit(
            "mac-sessions".into(),
            "iq2_xs".into(),
            Priority::Interactive,
            LIMIT,
        )
        .await;
    let Admission::Forward(request) = admitted else {
        panic!("the request was not forwarded: {admitted:?}");
    };
    (id, request)
}

/// Two readings of `watts` ten seconds apart, so the lease in use takes `watts * 10` joules
async fn two_readings(paddock: &Paddock, watts: f64, usd_per_kwh: Option<f64>) {
    let at = Instant::now();
    for at in [at, at + Duration::from_secs(10)] {
        paddock
            .engine
            .metered(Metered {
                reading: Reading {
                    watts,
                    source: Source::Wall,
                },
                at,
                usd_per_kwh,
            })
            .await;
    }
}

async fn release(paddock: &Paddock, id: &str) -> reqwest::Response {
    let path = format!("/paddock/leases/{id}");
    paddock
        .send(reqwest::Method::DELETE, &path, "k-mac", None)
        .await
}

#[tokio::test]
async fn a_release_answers_with_its_energy_headers() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (id, _request) = in_use(&paddock).await;
        // 360 W for ten seconds is one watt-hour, which at 10 cents a kWh costs a hundredth of a cent.
        two_readings(&paddock, 360.0, Some(0.10)).await;

        let released = release(&paddock, &id).await;

        assert_eq!(released.status(), 204);
        assert_eq!(released.headers()["paddock-energy-wh"], "1.000");
        assert_eq!(released.headers()["paddock-energy-usd"], "0.000100");
        assert_eq!(released.headers()["paddock-energy-source"], "wall");
    })
    .await;
}

#[tokio::test]
async fn an_unpriced_host_sends_no_dollars_header() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (id, _request) = in_use(&paddock).await;
        two_readings(&paddock, 360.0, None).await;

        let released = release(&paddock, &id).await;

        assert_eq!(released.status(), 204);
        assert_eq!(released.headers()["paddock-energy-wh"], "1.000");
        assert!(!released.headers().contains_key("paddock-energy-usd"));
        assert_eq!(released.headers()["paddock-energy-source"], "wall");
    })
    .await;
}

#[tokio::test]
async fn no_meter_sends_no_energy_headers() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (id, _request) = in_use(&paddock).await;

        let released = release(&paddock, &id).await;

        assert_eq!(released.status(), 204);
        for header in [
            "paddock-energy-wh",
            "paddock-energy-usd",
            "paddock-energy-source",
        ] {
            assert!(!released.headers().contains_key(header), "{header}");
        }
    })
    .await;
}
