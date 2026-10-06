//! Reclaimable leases, idle release and notes, through the engine on a paused clock.

use jiff::SignedDuration;

use super::{
    restart::{read_state, state_in},
    *,
};
use crate::book::Ended;

fn reclaimable_on(model: &str) -> LeaseRequest {
    LeaseRequest {
        reclaimable: true,
        ..lease_on(model, Hold::Connection)
    }
}

fn released_after(model: &str, seconds: u64) -> LeaseRequest {
    LeaseRequest {
        release_if_idle: Some(Duration::from_secs(seconds)),
        ..lease_on(model, Hold::Connection)
    }
}

const HALF_HOUR: Duration = Duration::from_secs(1_800);

#[tokio::test(start_paused = true)]
async fn a_reclaimable_lease_ends_reclaimed_when_its_model_is_evicted() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), reclaimable_on("iq2_xs")),
            )
            .await
            .expect("the engine took the ask");
            let _lease = granted(&mut events).await;

            let admitted = timeout(BOUND, admit(engine.clone(), "iq3_s")).await;
            assert!(
                matches!(admitted, Ok(Admission::Forward(_))),
                "{admitted:?}"
            );
            assert_eq!(
                timeout(BOUND, events.recv()).await,
                Ok(Some(LeaseEvent::Ended(Ended::Reclaimed)))
            );
            assert_eq!(
                timeout(SOON, events.recv()).await,
                Ok(None),
                "the stream did not end"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_reclaimable_leases_model_that_crashes_ends_it_and_is_not_loaded_again() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), reclaimable_on("laya")),
            )
            .await
            .expect("the engine took the ask");
            let _lease = granted(&mut events).await;
            feed.send(crash("laya", ProcessKind::Exit, false))
                .expect("the engine subscribed");

            assert_eq!(
                timeout(BOUND, events.recv()).await,
                Ok(Some(LeaseEvent::Ended(Ended::Reclaimed)))
            );
            until_state(&engine, "laya", State::Unloaded).await;
            sleep(SOON).await;
            assert_eq!(
                calls_of(&shepherd, &Call::Restart("laya".into())),
                1,
                "loaded for the lease, not again"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_reclaimable_lease_keeps_its_model_past_its_idle_time() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), reclaimable_on("laya")),
            )
            .await
            .expect("the engine took the ask");
            let _lease = granted(&mut events).await;
            sleep(Duration::from_secs(9 * 3_600)).await;
            assert_eq!(
                timeout(BOUND, state_of(&engine, "laya")).await,
                Ok(Some(State::Loaded)),
                "laya's idle is 8h"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_lease_that_asked_ends_idle_on_its_stream() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), released_after("laya", 1_800)),
            )
            .await
            .expect("the engine took the ask");
            let lease = granted(&mut events).await;
            assert!(
                timeout(HALF_HOUR - SOON, events.recv()).await.is_err(),
                "ended early"
            );
            assert_eq!(
                timeout(SOON * 2, events.recv()).await,
                Ok(Some(LeaseEvent::Ended(Ended::Idle { after: HALF_HOUR })))
            );
            assert_eq!(
                timeout(BOUND, engine.note(BENCH.into(), lease, "late".to_owned())).await,
                Ok(Err(LeaseRefused::NotFound))
            );
        },
    )
    .await;
}

/// A request is use for its whole length, so an hour-long one outlasts the half hour.
#[tokio::test(start_paused = true)]
async fn a_request_from_the_holder_keeps_its_lease_from_going_idle() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), released_after("laya", 1_800)),
            )
            .await
            .expect("the engine took the ask");
            let _lease = granted(&mut events).await;
            let admitted = timeout(
                BOUND,
                engine.admit(BENCH.into(), "laya".into(), Priority::Interactive, MAX_WAIT),
            )
            .await;
            let Ok(Admission::Forward(in_flight)) = admitted else {
                panic!("not forwarded: {admitted:?}");
            };
            assert!(
                timeout(Duration::from_secs(3_600), events.recv())
                    .await
                    .is_err(),
                "ended mid-request"
            );

            drop(in_flight);
            assert!(
                timeout(HALF_HOUR - SOON, events.recv()).await.is_err(),
                "ended before half an hour unused"
            );
            assert_eq!(
                timeout(SOON * 2, events.recv()).await,
                Ok(Some(LeaseEvent::Ended(Ended::Idle { after: HALF_HOUR })))
            );
        },
    )
    .await;
}

/// Without the note at 50 s, the 60 s lease would have expired by 100 s.
#[tokio::test(start_paused = true)]
async fn a_note_renews_a_heartbeat_lease_and_shows_in_the_status() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let ttl = Duration::from_secs(60);
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), lease_on("laya", Hold::Heartbeat { ttl })),
            )
            .await
            .expect("the engine took the ask");
            let lease = granted(&mut events).await;
            sleep(Duration::from_secs(50)).await;
            assert_eq!(
                timeout(
                    BOUND,
                    engine.note(BENCH.into(), lease, "step 412/900".to_owned())
                )
                .await,
                Ok(Ok(()))
            );
            sleep(Duration::from_secs(50)).await;

            let snapshot = timeout(BOUND, engine.snapshot())
                .await
                .expect("the engine answered");
            let view = snapshot
                .leases
                .iter()
                .find(|view| view.id == lease)
                .expect("the note renewed it");
            assert_eq!(view.note.as_deref(), Some("step 412/900"));
            assert_eq!(
                timeout(BOUND, engine.note(MAC.into(), lease, "from mac".to_owned())).await,
                Ok(Err(LeaseRefused::NotYours))
            );
            assert_eq!(
                timeout(
                    BOUND,
                    engine.note(BENCH.into(), LeaseId(999), "x".to_owned())
                )
                .await,
                Ok(Err(LeaseRefused::NotFound))
            );

            let snapshot = timeout(BOUND, engine.snapshot())
                .await
                .expect("the engine answered");
            let view = snapshot.leases.iter().find(|view| view.id == lease);
            assert_eq!(
                view.and_then(|view| view.note.as_deref()),
                Some("step 412/900"),
                "a refused note landed"
            );
            assert_eq!(
                timeout(Duration::from_secs(15), events.recv()).await,
                Ok(Some(LeaseEvent::Ended(Ended::Expired))),
                "a refused note renewed it"
            );
        },
    )
    .await;
}

/// The grant's save is 10 s old when the note comes, well inside the minute a request waits.
#[tokio::test(start_paused = true)]
async fn a_note_reaches_state_json_at_once() {
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
            let mut events = timeout(
                BOUND,
                engine.take_lease(BENCH.into(), lease_on("laya", Hold::Connection)),
            )
            .await
            .expect("the engine took the ask");
            let lease = granted(&mut events).await;
            sleep(Duration::from_secs(10)).await;
            assert_eq!(
                timeout(
                    BOUND,
                    engine.note(BENCH.into(), lease, "step 412/900".to_owned())
                )
                .await,
                Ok(Ok(()))
            );

            let saved = read_state(&path).leases.remove(0);
            assert_eq!(saved.note.as_deref(), Some("step 412/900"));
            let noted = saved.since + SignedDuration::from_secs(10);
            assert!(
                saved.last_activity.is_some_and(|at| at >= noted),
                "{:?} against {noted}",
                saved.last_activity
            );
        },
    )
    .await;
}
