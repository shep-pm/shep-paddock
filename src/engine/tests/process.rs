//! Backends going down, the subscription that says so, and unloads.

use super::*;

#[tokio::test(start_paused = true)]
async fn an_unexpected_exit_stops_the_sheep() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);

            feed.send(crash("iq2_xs", ProcessKind::Exit, false))
                .expect("the engine subscribed");
            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
            until_state(&engine, "iq2_xs", State::Unloaded).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_nobody_asked_for_is_read_as_a_crash() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "laya").await);

            feed.send(crash("laya", ProcessKind::Stop, true))
                .expect("the engine subscribed");
            until_called(&shepherd, Call::Stop("laya".into())).await;
            until_state(&engine, "laya", State::Unloaded).await;
        },
    )
    .await;
}

/// iq2_xs-256k runs on iq2_xs's sheep, so a misread Stop from evicting iq2_xs
/// would take down the model that replaced it.
#[tokio::test(start_paused = true)]
async fn the_engines_own_stop_is_not_read_as_a_crash() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            drop(forwarded(&engine, "iq2_xs-256k").await);
            assert_eq!(stops(&shepherd), 1);

            feed.send(crash("iq2_xs", ProcessKind::Stop, true))
                .expect("the engine subscribed");
            sleep(Duration::from_secs(5)).await;

            assert_eq!(state_of(&engine, "iq2_xs-256k").await, Some(State::Loaded));
            assert_eq!(stops(&shepherd), 1, "its own stop was read as a crash");
        },
    )
    .await;
}

/// The eviction's Stop event never came, so only the next start says the mark is stale.
#[tokio::test(start_paused = true)]
async fn a_start_clears_the_mark_so_a_later_crash_is_read() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            drop(forwarded(&engine, "iq2_xs-256k").await);

            feed.send(crash("iq2_xs", ProcessKind::Started, true))
                .expect("the engine subscribed");
            feed.send(crash("iq2_xs", ProcessKind::Stop, false))
                .expect("the engine subscribed");
            until("the crash stopping the sheep", || async {
                stops(&shepherd) == 2
            })
            .await;
            until_state(&engine, "iq2_xs-256k", State::Unloaded).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_lost_event_stream_is_resubscribed() {
    let shepherd = FakeShepherd::new();
    let first = shepherd.feed();
    let second = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            assert_eq!(shepherd.subscriptions(), 1);

            drop(first);
            until("a second subscription", || async {
                shepherd.subscriptions() == 2
            })
            .await;
            second
                .send(crash("iq2_xs", ProcessKind::Exit, false))
                .expect("the engine subscribed again");
            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_model_removed_from_the_config_unloads_as_the_model_it_loaded_as() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "laya").await);

            let without_laya = SHEEP_MODELS
                .split("[models.laya]")
                .next()
                .unwrap_or_default();
            engine.reconfigure(config(without_laya)).await;

            until_called(&shepherd, Call::Stop("laya".into())).await;
            until("laya leaving the book", || async {
                state_of(&engine, "laya").await.is_none()
            })
            .await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_subscription_is_tried_again_a_second_later() {
    let shepherd = FakeShepherd::new();
    shepherd.refuse_subscription();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let started = Instant::now();
            until("a second subscription", || async {
                shepherd.subscriptions() == 2
            })
            .await;
            let waited = started.elapsed();
            assert!(
                waited >= Duration::from_secs(1) && waited < Duration::from_millis(1100),
                "subscribed again after {waited:?}"
            );

            drop(forwarded(&engine, "iq2_xs").await);
            feed.send(crash("iq2_xs", ProcessKind::Exit, false))
                .expect("the engine subscribed again");
            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
        },
    )
    .await;
}

/// The model stays Unloading while its stop fails, so nothing loads into room it still holds.
#[tokio::test(start_paused = true)]
async fn a_failed_unload_is_tried_again_until_it_is_done() {
    let shepherd = FakeShepherd::failing_stops(1);
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            let iq3_s = spawn_local(admit(engine.clone(), "iq3_s"));
            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
            let failed = Instant::now();
            sleep(Duration::from_secs(1)).await;
            assert_eq!(state_of(&engine, "iq2_xs").await, Some(State::Unloading));

            let admitted = timeout(BOUND, iq3_s).await.expect("iq3_s is answered");
            assert!(
                matches!(admitted, Ok(Admission::Forward(_))),
                "{admitted:?}"
            );
            // `failed` was read up to one poll after the first stop, and the retry waits 5 s.
            let retried = failed.elapsed();
            assert!(
                retried >= Duration::from_millis(4980) && retried <= Duration::from_secs(5),
                "tried again after {retried:?}"
            );
            assert_eq!(
                shepherd.calls(),
                vec![
                    Call::SetEnv("iq2_xs".into(), "CONTEXT".into(), "131072".into()),
                    Call::Restart("iq2_xs".into()),
                    Call::Stop("iq2_xs".into()),
                    Call::Stop("iq2_xs".into()),
                    Call::Restart("iq3_s".into()),
                ]
            );
        },
    )
    .await;
}
