//! Leases: ids, owners, holders hanging up and attaching again.

use super::*;

#[tokio::test(start_paused = true)]
async fn renewing_or_releasing_another_clients_lease_is_refused() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let ttl = Duration::from_secs(60);
            let mut events = engine
                .take_lease(MAC.into(), lease_on("laya", Hold::Heartbeat { ttl }))
                .await;
            let lease = granted(&mut events).await;

            assert_eq!(
                engine.renew(BENCH.into(), lease).await,
                Err(LeaseRefused::NotYours)
            );
            assert_eq!(
                engine.release(BENCH.into(), lease).await,
                Err(LeaseRefused::NotYours)
            );
            assert_eq!(
                engine.renew(MAC.into(), LeaseId(999)).await,
                Err(LeaseRefused::NotFound)
            );
            assert_eq!(engine.renew(MAC.into(), lease).await, Ok(()));
            assert_eq!(engine.release(MAC.into(), lease).await, Ok(()));
            assert_eq!(
                timeout(BOUND, events.recv()).await,
                Ok(Some(LeaseEvent::Ended(crate::book::Ended::Released)))
            );
            assert_eq!(
                engine.release(MAC.into(), lease).await,
                Err(LeaseRefused::NotFound)
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_holder_detaches_and_attach_resumes() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = engine
                .take_lease(MAC.into(), lease_on("laya", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;

            drop(events);
            until("the holder detaching", || async {
                engine
                    .snapshot()
                    .await
                    .leases
                    .first()
                    .is_some_and(|l| !l.attached)
            })
            .await;

            let mut again = engine
                .attach(MAC.into(), lease)
                .await
                .expect("the owner attaches");
            assert_eq!(
                timeout(BOUND, again.recv()).await,
                Ok(Some(LeaseEvent::Granted { lease }))
            );
            assert!(engine.snapshot().await.leases[0].attached);
            assert_eq!(
                engine.attach(MAC.into(), lease).await.err(),
                Some(LeaseRefused::Attached)
            );
            assert_eq!(
                engine.attach(BENCH.into(), lease).await.err(),
                Some(LeaseRefused::NotYours)
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_heartbeat_holder_that_drops_its_stream_stays_attached() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let ttl = Duration::from_secs(60);
            let mut events = engine
                .take_lease(MAC.into(), lease_on("laya", Hold::Heartbeat { ttl }))
                .await;
            let lease = granted(&mut events).await;

            drop(events);
            sleep(Duration::from_secs(5)).await;
            assert_eq!(engine.renew(MAC.into(), lease).await, Ok(()));
            assert!(engine.snapshot().await.leases[0].attached);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_lease_on_an_unknown_model_fails() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = engine
                .take_lease(MAC.into(), lease_on("ghost", Hold::Connection))
                .await;
            assert_eq!(
                timeout(BOUND, events.recv()).await,
                Ok(Some(LeaseEvent::Failed("no model named ghost".into())))
            );
            assert!(matches!(
                admit(engine.clone(), "ghost").await,
                Admission::Unknown
            ));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_lease_asker_that_leaves_while_waiting_is_removed_from_the_book() {
    let shepherd = FakeShepherd::stalling_restart();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let events = engine
                .take_lease(MAC.into(), lease_on("laya", Hold::Connection))
                .await;
            until("the lease queueing", || async {
                engine.snapshot().await.waiters.len() == 1
            })
            .await;

            drop(events);
            until_within(SOON, "the lease leaving the book", || async {
                engine.snapshot().await.waiters.is_empty()
            })
            .await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn lease_ids_start_past_every_restored_lease() {
    let mut engine = engine();
    assert_eq!(engine.next_lease(), LeaseId(1));

    let now = engine.clock.moment();
    let ask = LeaseAsk {
        lease: LeaseId(7),
        client: MAC.into(),
        model: "laya".into(),
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: None,
    };
    let _ = engine
        .book
        .restore(now, Vec::new(), vec![RestoredLease { ask, since: now }]);

    assert_eq!(engine.next_lease(), LeaseId(8));
    assert_eq!(engine.next_lease(), LeaseId(9));
}
