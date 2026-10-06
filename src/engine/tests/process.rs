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
            until("a listing after the second subscription", || async {
                shepherd.subscriptions() == 2 && shepherd.listings() == 2
            })
            .await;
            sleep(Duration::from_secs(1)).await;
            assert_eq!(stops(&shepherd), 0, "a running sheep was read as crashed");
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

#[tokio::test(start_paused = true)]
async fn the_flock_is_listed_once_subscribed_at_start() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            // Held so the engine runs on: it stops once every handle is gone.
            let _engine = engine;
            until_within(SOON, "a listing at start", || async {
                shepherd.subscriptions() == 1 && shepherd.listings() == 1
            })
            .await;
        },
    )
    .await;
}

/// The stream is down for a second, iq2_xs crashes in it, and no event says so.
#[tokio::test(start_paused = true)]
async fn a_crash_while_the_stream_is_down_is_found_on_resubscribe() {
    let shepherd = FakeShepherd::new();
    let first = shepherd.feed();
    shepherd.refuse_subscription();
    let third = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let _open = third;
            drop(forwarded(&engine, "iq2_xs").await);

            drop(first);
            until("the refused subscription", || async {
                shepherd.subscriptions() == 2
            })
            .await;
            shepherd.crash("iq2_xs");
            assert_eq!(stops(&shepherd), 0);

            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
            until_state(&engine, "iq2_xs", State::Unloaded).await;
            assert_eq!(shepherd.subscriptions(), 3);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_listing_taken_before_a_reload_is_not_read_against_it() {
    let mut engine = engine();
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "laya".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    engine.feed(Event::Loaded {
        model: "laya".into(),
    });
    let _ = engine.take_jobs();
    let before = engine.expected_running();
    assert_eq!(before.len(), 1);

    let mut queue = VecDeque::new();
    engine.apply(vec![Action::Load("laya".into())], &mut queue);
    let _ = engine.take_jobs();
    engine.reconcile(before, &[]);
    assert!(
        engine.take_jobs().is_empty(),
        "a stale listing unloaded laya"
    );

    let now = engine.expected_running();
    engine.reconcile(now, &[]);
    assert!(
        matches!(engine.take_jobs().as_slice(), [Job::Unload(model)] if model.name == ModelName::from("laya"))
    );
}

/// Two exits while iq3_s loads end the book's retry. The second load is
/// stopped, so it never holds memory the book counts free, even once its
/// restart answers.
#[tokio::test(start_paused = true)]
async fn a_load_the_book_gave_up_on_is_stopped_even_if_it_comes_up() {
    let shepherd = FakeShepherd::gated_restart();
    let feed = shepherd.feed();
    with_engine(config(SHEEP_MODELS), shepherd.clone(), |engine| async move {
        let restart = Call::Restart("iq3_s".into());
        let iq3_s = spawn_local(admit(engine.clone(), "iq3_s"));
        until("the first restart", || async { calls_of(&shepherd, &restart) == 1 }).await;
        feed.send(crash("iq3_s", ProcessKind::Exit, false))
            .expect("the engine subscribed");
        until("the retry's restart", || async { calls_of(&shepherd, &restart) == 2 }).await;
        feed.send(crash("iq3_s", ProcessKind::Exit, false))
            .expect("the engine subscribed");

        let admitted = timeout(BOUND, iq3_s).await.expect("iq3_s is answered");
        assert!(
            matches!(&admitted, Ok(Admission::Failed(error)) if error == "backend exited while loading"),
            "{admitted:?}"
        );
        // Well inside the load timeout, whose cleanup would stop it too.
        until_within(SOON, "iq3_s stopped", || async {
            shepherd.calls().contains(&Call::Stop("iq3_s".into()))
        })
        .await;
        shepherd.open_gate();
        shepherd.open_gate();
        sleep(Duration::from_secs(5)).await;
        assert_eq!(state_of(&engine, "iq3_s").await, Some(State::Unloaded));
        assert_eq!(calls_of(&shepherd, &restart), 2);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_load_that_comes_up_after_the_book_gave_up_is_stopped() {
    let mut engine = engine();
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "laya".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    for _ in 0..2 {
        engine.feed(Event::LoadFailed {
            model: "laya".into(),
            error: "refused".into(),
        });
    }
    let _ = engine.take_jobs();
    assert_eq!(engine.book.state(&"laya".into()), Some(State::Unloaded));

    engine.finished("laya".into(), Outcome::Loaded);
    assert!(
        matches!(engine.take_jobs().as_slice(), [Job::Unload(model)] if model.name == ModelName::from("laya"))
    );
    engine.finished("laya".into(), Outcome::Unloaded);
    assert_eq!(engine.book.state(&"laya".into()), Some(State::Unloaded));
}

/// The eviction of iq2_xs left a mark no Stop event cleared, and its next
/// start fell in a subscription gap, so only the listing can see the crash.
#[tokio::test(start_paused = true)]
async fn a_stale_stop_mark_does_not_hide_a_crash_from_the_listing() {
    let shepherd = FakeShepherd::new();
    let first = shepherd.feed();
    shepherd.refuse_subscription();
    let third = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let _open = third;
            let stop = Call::Stop("iq2_xs".into());
            drop(forwarded(&engine, "iq2_xs").await);
            drop(forwarded(&engine, "iq3_s").await);
            assert_eq!(calls_of(&shepherd, &stop), 1);

            drop(first);
            until("the refused subscription", || async {
                shepherd.subscriptions() == 2
            })
            .await;
            drop(forwarded(&engine, "iq2_xs").await);
            shepherd.crash("iq2_xs");

            until("the crash stopping iq2_xs", || async {
                calls_of(&shepherd, &stop) == 2
            })
            .await;
            until_state(&engine, "iq2_xs", State::Unloaded).await;
        },
    )
    .await;
}

/// Each stream ends as soon as it opens, at 0 s, 1 s and 2 s.
#[tokio::test(start_paused = true)]
async fn a_stream_that_ends_at_once_is_not_resubscribed_in_a_tight_loop() {
    let shepherd = FakeShepherd::new();
    for _ in 0..10 {
        drop(shepherd.feed());
    }
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            // Held so the engine runs on: it stops once every handle is gone.
            let _engine = engine;
            sleep(Duration::from_millis(2500)).await;
            assert_eq!(shepherd.subscriptions(), 3);
        },
    )
    .await;
}

/// laya's load gives up on its second crash, and the room goes to laya-b on the
/// same sheep in the same event, so the quiet stop must not replace laya-b's load.
#[tokio::test(start_paused = true)]
async fn a_quiet_stop_leaves_a_load_queued_on_its_sheep() {
    let laya_b = r#"
[models.laya-b]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;
    let shared = format!("{SHEEP_MODELS}{laya_b}");
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(config(&shared), Clock::new(), notify);
    for (waiter, model) in [(1, "laya"), (2, "laya-b")] {
        engine.feed(Event::RequestArrived {
            waiter: WaiterId(waiter),
            client: MAC.into(),
            model: model.into(),
            priority: Priority::Interactive,
            max_wait: MAX_WAIT,
        });
    }
    let _ = engine.take_jobs();

    engine.process(crash("laya", ProcessKind::Exit, false));
    let _ = engine.take_jobs();
    engine.process(crash("laya", ProcessKind::Exit, false));

    let jobs = engine.take_jobs();
    assert!(
        matches!(jobs.as_slice(), [Job::Load(model)] if model.name == ModelName::from("laya-b")),
        "{jobs:?}"
    );
}
