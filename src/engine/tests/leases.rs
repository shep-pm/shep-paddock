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
                timeout(SOON, events.recv()).await,
                Ok(None),
                "the stream did not end"
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
            assert_eq!(
                timeout(SOON, events.recv()).await,
                Ok(None),
                "the stream did not end"
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
        reclaimable: false,
        release_if_idle: None,
    };
    let _ = engine.book.restore(
        now,
        Vec::new(),
        &[],
        vec![RestoredLease {
            ask,
            since: now,
            last_activity: None,
        }],
    );

    assert_eq!(engine.next_lease(), LeaseId(8));
    assert_eq!(engine.next_lease(), LeaseId(9));
}

#[tokio::test(start_paused = true)]
async fn a_refused_lease_stream_ends_after_its_refusal() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut held = engine
                .take_lease(BENCH.into(), lease_on("iq2_xs", Hold::Connection))
                .await;
            let _lease = granted(&mut held).await;

            let mut ask = lease_on("iq3_s", Hold::Connection);
            ask.max_wait = Some(MAX_WAIT);
            let mut events = engine.take_lease(MAC.into(), ask).await;
            assert!(matches!(
                timeout(BOUND, events.recv()).await,
                Ok(Some(LeaseEvent::Refused(_)))
            ));
            assert_eq!(
                timeout(SOON, events.recv()).await,
                Ok(None),
                "the stream did not end"
            );
        },
    )
    .await;
}

/// The holder reads nothing while laya loads, so its stream is full when the grant comes.
#[tokio::test(start_paused = true)]
async fn a_grant_arrives_on_a_stream_full_of_waits() {
    let mut engine = engine();
    let (events, mut heard) = lease_channel();
    for _ in 0..40 {
        events.send(LeaseEvent::Waiting {
            reason: Reason::Loading {
                model: "laya".into(),
            },
            estimate: None,
        });
    }
    engine.command(super::super::Command::TakeLease {
        waiter: WaiterId(1),
        client: BENCH.into(),
        ask: lease_on("laya", Hold::Connection),
        events,
    });
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);

    let mut last = None;
    while let Ok(Some(event)) = timeout(SOON, heard.recv()).await {
        last = Some(event);
    }
    assert_eq!(last, Some(LeaseEvent::Granted { lease: LeaseId(1) }));
}

/// laya is loaded, so a take would be granted in the same step that hears it.
#[tokio::test(start_paused = true)]
async fn a_take_whose_asker_has_already_gone_is_never_granted() {
    let mut engine = engine();
    let (events, _heard) = lease_channel();
    engine.command(super::super::Command::TakeLease {
        waiter: WaiterId(1),
        client: BENCH.into(),
        ask: lease_on("laya", Hold::Connection),
        events,
    });
    let _ = engine.take_jobs();
    engine.finished("laya".into(), Outcome::Loaded);
    let ttl = Duration::from_secs(60);
    let (events, heard) = lease_channel();
    drop(heard);

    engine.command(super::super::Command::TakeLease {
        waiter: WaiterId(2),
        client: MAC.into(),
        ask: lease_on("laya", Hold::Heartbeat { ttl }),
        events,
    });

    let holders: Vec<_> = engine
        .snapshot()
        .leases
        .into_iter()
        .map(|lease| lease.client)
        .collect();
    assert_eq!(holders, [crate::config::ClientName::from(BENCH)]);
}
