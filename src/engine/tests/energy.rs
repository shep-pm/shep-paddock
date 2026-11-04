//! A meter reading's energy, split among the leases with a request in flight.

use super::*;
use crate::{
    engine::{Metered, Share, Use},
    meter::{Reading, Source},
};

const HOUR: Duration = Duration::from_secs(3600);

fn wall(watts: f64) -> Reading {
    Reading {
        watts,
        source: Source::Wall,
    }
}

fn metered(watts: f64, at: Instant, usd_per_kwh: Option<f64>) -> Metered {
    Metered {
        reading: wall(watts),
        at,
        usd_per_kwh,
    }
}

fn almost(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got} is not {want}");
}

/// A lease on laya held by `client`, with the stream that keeps it
async fn laya_for(engine: &EngineHandle, client: &str) -> (LeaseId, LeaseEvents) {
    let mut events = engine
        .take_lease(client.into(), lease_on("laya", Hold::Connection))
        .await;
    (granted(&mut events).await, events)
}

#[tokio::test(start_paused = true)]
async fn a_reading_goes_to_the_lease_with_a_request_in_flight() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;
            let _request = forwarded(&engine, "laya").await;
            let start = Instant::now();

            engine.metered(metered(360.0, start, None)).await;
            let shares = engine
                .metered(metered(360.0, start + SOON * 10, None))
                .await;

            // 360 W for ten seconds is 3600 J, an hour of one watt.
            assert_eq!(
                shares,
                [Share {
                    model: Some("laya".into()),
                    used: Use::Lease,
                    joules: 3600.0
                }]
            );
            let used = engine.release(MAC.into(), lease).await;
            let used = used.expect("released").expect("a meter ran");
            almost(used.wh, 1.0);
            assert_eq!(used.source, Source::Wall);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn two_leases_in_flight_split_a_reading_evenly() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (mac, _mac_events) = laya_for(&engine, MAC).await;
            let (bench, _bench_events) = laya_for(&engine, BENCH).await;
            let _mac_request = forwarded(&engine, "laya").await;
            let bench_request = engine
                .admit(BENCH.into(), "laya".into(), Priority::Batch, MAX_WAIT)
                .await;
            assert!(matches!(bench_request, Admission::Forward(_)));
            let start = Instant::now();

            engine.metered(metered(720.0, start, None)).await;
            let shares = engine
                .metered(metered(720.0, start + SOON * 10, None))
                .await;

            assert_eq!(
                shares,
                [Share {
                    model: Some("laya".into()),
                    used: Use::Lease,
                    joules: 7200.0
                }]
            );
            for (client, lease) in [(MAC, mac), (BENCH, bench)] {
                let used = engine.release(client.into(), lease).await;
                almost(used.expect("released").expect("a meter ran").wh, 1.0);
            }
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_reading_with_nothing_in_flight_is_idle() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;
            let start = Instant::now();

            engine.metered(metered(360.0, start, None)).await;
            let shares = engine
                .metered(metered(360.0, start + SOON * 10, None))
                .await;

            assert_eq!(
                shares,
                [Share {
                    model: None,
                    used: Use::Idle,
                    joules: 3600.0
                }]
            );
            let used = engine.release(MAC.into(), lease).await;
            almost(used.expect("released").expect("a meter ran").wh, 0.0);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn dollars_follow_the_price_of_each_reading() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;
            let _request = forwarded(&engine, "laya").await;
            let start = Instant::now();

            engine.metered(metered(1000.0, start, Some(0.10))).await;
            engine
                .metered(metered(1000.0, start + HOUR, Some(0.10)))
                .await;
            engine
                .metered(metered(1000.0, start + HOUR * 2, Some(0.20)))
                .await;

            // One kWh at ten cents, then one at twenty.
            let used = engine.release(MAC.into(), lease).await;
            let used = used.expect("released").expect("a meter ran");
            almost(used.wh, 2000.0);
            almost(used.usd.expect("priced"), 0.30);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unpriced_host_gives_no_dollars() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;
            let _request = forwarded(&engine, "laya").await;
            let start = Instant::now();

            engine.metered(metered(360.0, start, None)).await;
            engine
                .metered(metered(360.0, start + SOON * 10, None))
                .await;

            let used = engine.release(MAC.into(), lease).await;
            let used = used.expect("released").expect("a meter ran");
            assert_eq!(used.usd, None);
            almost(used.wh, 1.0);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_meter_gives_no_energy() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;

            let snapshot = engine.snapshot().await;
            assert_eq!(snapshot.leases[0].energy_mwh, None);
            assert_eq!(engine.release(MAC.into(), lease).await, Ok(None));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_snapshot_shows_the_energy_so_far_in_milliwatt_hours() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let (lease, _events) = laya_for(&engine, MAC).await;
            let (idle, _idle_events) = laya_for(&engine, BENCH).await;
            let _request = forwarded(&engine, "laya").await;
            let start = Instant::now();

            engine.metered(metered(360.0, start, None)).await;
            engine.metered(metered(360.0, start + SOON * 5, None)).await;

            let snapshot = engine.snapshot().await;
            let energy = |id| {
                let view = snapshot.leases.iter().find(|view| view.id == id);
                view.map(|view| view.energy_mwh)
            };
            // Half a watt-hour for the lease in use, none for the other.
            assert_eq!(energy(lease), Some(Some(500)));
            assert_eq!(energy(idle), Some(Some(0)));
        },
    )
    .await;
}
