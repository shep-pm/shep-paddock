//! What reaches `state.json` between lease changes: request activity, and each model holding memory.

use jiff::{SignedDuration, Timestamp};

use super::{
    restart::{bench_lease, read_state, saved_with, state_in},
    *,
};
use crate::saved::{SavedHold, SavedLease};

/// 25 of the lease's 30 idle minutes passed before the restart, so 5 are left after it.
#[tokio::test(start_paused = true)]
async fn a_restart_keeps_a_leases_idle_clock() {
    let now = Timestamp::now();
    let lease = SavedLease {
        hold: SavedHold::Heartbeat { ttl_ms: 3_600_000 },
        last_activity: Some(now - SignedDuration::from_mins(25)),
        release_if_idle_ms: Some(1_800_000),
        ..bench_lease(7, "laya", SavedHold::Connection {})
    };
    let start = Start {
        saved: saved_with(&[], vec![lease]),
        ..Start::default()
    };
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        start,
        |engine| async move {
            sleep(Duration::from_secs(4 * 60)).await;
            assert_eq!(engine.snapshot().await.leases.len(), 1);
            sleep(Duration::from_secs(2 * 60)).await;
            assert!(
                engine.snapshot().await.leases.is_empty(),
                "it ends 5 minutes after the restart"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_request_from_a_holder_reaches_state_json_within_a_minute() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let start = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        start,
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let _lease = granted(&mut events).await;
            sleep(Duration::from_secs(120)).await;
            let admitted = timeout(
                BOUND,
                engine.admit(BENCH.into(), "laya".into(), Priority::Interactive, MAX_WAIT),
            )
            .await;
            let Ok(Admission::Forward(in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };
            drop(in_flight);

            let lease = read_state(&path).leases.remove(0);
            let used = lease.last_activity.expect("saved");
            assert!(
                used >= lease.since + SignedDuration::from_secs(120),
                "{used} against {}",
                lease.since
            );
        },
    )
    .await;
}
