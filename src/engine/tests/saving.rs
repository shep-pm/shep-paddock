//! Lease activity reaching `state.json` between lease changes, and coming back after a restart.

use jiff::{SignedDuration, Timestamp};

use super::{
    restart::{bench_lease, read_state, saved_with, state_in},
    *,
};
use crate::{
    discover::Discovered,
    saved::{self, SavedHold, SavedLease},
};

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
            let Ok(Admission::Forward(_in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };

            let lease = read_state(&path).leases.remove(0);
            assert_eq!(lease.last_activity, None, "not saved as in use");
        },
    )
    .await;
}

/// The grant's save is 10 s old when the request comes, so the request waits on a later save.
#[tokio::test(start_paused = true)]
async fn a_request_soon_after_a_save_reaches_state_json_within_a_minute() {
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
            let at_grant = read_state(&path).leases.remove(0);
            sleep(Duration::from_secs(10)).await;
            let admitted = timeout(
                BOUND,
                engine.admit(BENCH.into(), "laya".into(), Priority::Interactive, MAX_WAIT),
            )
            .await;
            let Ok(Admission::Forward(_in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };

            sleep(Duration::from_secs(49)).await;
            assert_eq!(read_state(&path).leases[0], at_grant, "saved early");
            sleep(Duration::from_secs(2)).await;
            assert_ne!(read_state(&path).leases[0], at_grant);
        },
    )
    .await;
}

/// Loading laya's sheep saves first, and the save fails.
#[tokio::test(start_paused = true)]
async fn a_failed_save_is_tried_again_a_minute_later() {
    let home = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        home.path().join("paddock"),
        "a file where the directory goes",
    )
    .expect("written");
    let mut engine = engine();
    engine.restore(Start {
        state: Some(state_in(home.path())),
        ..Start::default()
    });

    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "laya".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });

    assert_eq!(
        engine.next_deadline(),
        Some(Instant::now() + Duration::from_secs(60))
    );
}

/// A 40-minute generation under a 30-minute idle release, with the dog restarting 35 minutes in.
#[tokio::test(start_paused = true)]
async fn a_lease_in_use_at_a_restart_restores_as_used_then() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let first = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        first,
        |engine| async move {
            let ask = LeaseRequest {
                release_if_idle: Some(Duration::from_secs(30 * 60)),
                ..lease_on(
                    "laya",
                    Hold::Heartbeat {
                        ttl: Duration::from_secs(2 * 3600),
                    },
                )
            };
            let mut events = engine.take_lease(BENCH.into(), ask).await;
            let _lease = granted(&mut events).await;
            sleep(Duration::from_secs(120)).await;
            let admitted = timeout(
                BOUND,
                engine.admit(BENCH.into(), "laya".into(), Priority::Interactive, MAX_WAIT),
            )
            .await;
            let Ok(Admission::Forward(_in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };
            sleep(Duration::from_secs(33 * 60)).await;
            let clock = engine.clock();
            let _ = tx.send(clock.wall(clock.moment()));
        },
    )
    .await;
    let restarted_at = rx.await.expect("the first engine ran");

    let second = Start {
        saved: read_state(&path),
        ..Start::default()
    };
    let clock = Clock::started_at(restarted_at);
    with_engine_on(
        clock,
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        second,
        |engine| async move {
            assert_eq!(engine.snapshot().await.leases.len(), 1, "it ended idle");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_holders_last_request_ending_saves_at_once() {
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
            sleep(Duration::from_secs(10)).await;
            drop(in_flight);
            // Notices are read before commands, so the finish is in the book once this answers.
            let _ = engine.snapshot().await;

            let lease = read_state(&path).leases.remove(0);
            let ended = lease.since + SignedDuration::from_secs(130);
            assert!(
                lease.last_activity.is_some_and(|at| at >= ended),
                "{:?} against {ended}",
                lease.last_activity
            );
        },
    )
    .await;
}

/// The file is removed once bench-01's lease is saved, so any later write shows.
#[tokio::test(start_paused = true)]
async fn a_request_from_a_client_without_a_lease_writes_nothing() {
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
            std::fs::remove_file(&path).expect("removed");

            drop(forwarded(&engine, "laya").await);
            sleep(Duration::from_secs(120)).await;

            assert!(!path.exists(), "mac-sessions holds no lease");
        },
    )
    .await;
}

/// iq3_s is already loaded, so serving it changes nothing else the file holds.
#[tokio::test(start_paused = true)]
async fn a_holders_request_for_another_model_writes_nothing() {
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
            drop(forwarded(&engine, "iq3_s").await);
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let _lease = granted(&mut events).await;
            sleep(Duration::from_secs(120)).await;
            std::fs::remove_file(&path).expect("removed");

            let admitted = timeout(
                BOUND,
                engine.admit(
                    BENCH.into(),
                    "iq3_s".into(),
                    Priority::Interactive,
                    MAX_WAIT,
                ),
            )
            .await;
            let Ok(Admission::Forward(in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };
            drop(in_flight);
            sleep(Duration::from_secs(120)).await;

            assert!(!path.exists(), "bench-01's lease is on laya");
        },
    )
    .await;
}

/// A batch job under a lease: each request ends before the next, a second apart.
#[tokio::test(start_paused = true)]
async fn sequential_requests_from_a_holder_write_at_most_twice() {
    use std::os::unix::fs::MetadataExt;

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
            // Each write renames a new file over the old one, with a new inode and mtime.
            let inode = || {
                let meta = std::fs::metadata(&path).expect("the state file");
                (meta.ino(), meta.modified().expect("an mtime"))
            };
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let _lease = granted(&mut events).await;
            sleep(Duration::from_secs(120)).await;
            let mut inodes = vec![inode()];

            for _ in 0..10 {
                let admitted = timeout(
                    BOUND,
                    engine.admit(BENCH.into(), "laya".into(), Priority::Interactive, MAX_WAIT),
                )
                .await;
                let Ok(Admission::Forward(in_flight)) = admitted else {
                    panic!("not forwarded: {admitted:?}");
                };
                inodes.push(inode());
                drop(in_flight);
                // Notices are read before commands, so the finish is in the book once this answers.
                let _ = engine.snapshot().await;
                inodes.push(inode());
                sleep(Duration::from_secs(1)).await;
            }
            inodes.dedup();
            assert!(inodes.len() <= 3, "{} writes", inodes.len() - 1);

            sleep(Duration::from_secs(60)).await;
            let lease = read_state(&path).leases.remove(0);
            let last = lease.since + SignedDuration::from_secs(129);
            assert!(
                lease.last_activity.is_some_and(|at| at >= last),
                "{:?} against {last}",
                lease.last_activity
            );
        },
    )
    .await;
}

/// Saved in use, so no activity: unless the restart's moment is written, every restart resets it.
#[tokio::test(start_paused = true)]
async fn a_lease_saved_in_use_is_written_with_the_restart_as_its_activity() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let mut engine = engine();
    let clock = engine.clock;
    let laya = ModelName::from("laya");
    let mut saved = saved_with(
        &[("laya", "laya")],
        vec![bench_lease(7, "laya", SavedHold::Connection {})],
    );
    let unplaced = saved::SavedModel {
        placement: None,
        stray: false,
    };
    saved.models.insert(laya.clone(), unplaced);
    saved::store(&path, &saved).expect("stored");
    let footprint = config(SHEEP_MODELS).models[&laya].footprint;
    engine.restore(Start {
        state: Some(path.clone()),
        saved,
        discovered: Discovered {
            loaded: vec![crate::book::Found {
                model: laya,
                footprint,
                placement: None,
                stray: false,
            }],
            ..Discovered::default()
        },
        survey: None,
    });

    let lease = read_state(&path).leases.remove(0);
    assert_eq!(lease.last_activity, Some(clock.wall(clock.moment())));
}
