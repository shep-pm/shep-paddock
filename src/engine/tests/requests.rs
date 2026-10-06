//! Requests: forwarding once loaded, in-flight counts, waiting, leaving, and failed loads.

use super::*;

// Real time: the ready check is a real loopback socket.
#[tokio::test]
async fn a_request_for_a_cold_model_is_forwarded_once_it_is_ready() {
    let (base, server) = fake_http(vec![(
        "GET",
        "/health",
        vec![(503, "starting"), (200, r#"{"loaded":true}"#)],
    )]);
    let ready = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
ram = "5G"
idle = "8h"
"#
    ));
    let shepherd = FakeShepherd::new();
    with_engine(ready, shepherd.clone(), |engine| async move {
        let admitted = timeout(Duration::from_secs(10), admit(engine.clone(), "laya")).await;
        assert!(
            matches!(admitted, Ok(Admission::Forward(_))),
            "{admitted:?}"
        );
        assert_eq!(shepherd.calls(), vec![Call::Restart("laya".into())]);
        assert_eq!(server.seen().len(), 2, "forwarded before it was ready");
        let snapshot = engine.snapshot().await;
        assert_eq!(snapshot.models[0].state, State::Loaded);
        assert_eq!(snapshot.models[0].in_flight, 1);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn dropping_in_flight_lets_the_model_be_evicted() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let held = forwarded(&engine, "iq2_xs").await;
            let iq3_s = spawn_local(admit(engine.clone(), "iq3_s"));
            until_state(&engine, "iq2_xs", State::Evicting).await;
            sleep(Duration::from_secs(60)).await;
            assert_eq!(stops(&shepherd), 0, "stopped with a request in flight");

            drop(held);
            let admitted = timeout(BOUND, iq3_s).await.expect("iq3_s is answered");
            assert!(
                matches!(admitted, Ok(Admission::Forward(_))),
                "{admitted:?}"
            );
            assert_eq!(
                shepherd.calls(),
                vec![
                    Call::SetEnv("iq2_xs".into(), "CONTEXT".into(), "131072".into()),
                    Call::Restart("iq2_xs".into()),
                    Call::Stop("iq2_xs".into()),
                    Call::Restart("iq3_s".into()),
                ]
            );
        },
    )
    .await;
}

/// A lease with no expected end never frees its model, so a request for another waits for
/// nothing and is refused at once, naming the lease with a time the clock turns into wall time.
#[tokio::test(start_paused = true)]
async fn a_request_behind_an_endless_lease_is_refused_with_its_holder() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("iq2_xs", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;

            let admitted = timeout(BOUND, admit(engine.clone(), "iq3_s")).await;
            let Ok(Admission::Refused(refusal)) = admitted else {
                panic!("not refused: {admitted:?}");
            };
            let Reason::Held { since, .. } = refusal.reason else {
                panic!("not held: {:?}", refusal.reason);
            };
            assert_eq!(
                refusal.reason,
                Reason::Held {
                    model: "iq2_xs".into(),
                    client: BENCH.into(),
                    lease,
                    since,
                    until: None,
                    idle_since: Some(since),
                }
            );
            let granted_at = engine.clock().wall(since);
            let ago = jiff::Timestamp::now().duration_since(granted_at);
            assert!(
                ago >= jiff::SignedDuration::ZERO && ago < jiff::SignedDuration::from_secs(60),
                "granted {ago:?} ago"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_client_that_leaves_while_waiting_is_removed_from_the_book() {
    let shepherd = FakeShepherd::stalling_restart();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let waiting = spawn_local(admit(engine.clone(), "iq3_s"));
            until("the request queueing", || async {
                engine.snapshot().await.waiters.len() == 1
            })
            .await;

            waiting.abort();
            until_within(SOON, "the waiter leaving the book", || async {
                engine.snapshot().await.waiters.is_empty()
            })
            .await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_load_that_times_out_is_retried_once_then_fails() {
    let shepherd = FakeShepherd::stalling_restart();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let started = Instant::now();
            let admitted = timeout(BOUND, admit(engine.clone(), "iq3_s")).await;

            assert!(
                matches!(&admitted, Ok(Admission::Failed(error)) if error == "not ready after 5m"),
                "{admitted:?}"
            );
            let waited = started.elapsed();
            assert!(
                waited >= Duration::from_secs(600) && waited < Duration::from_secs(601),
                "failed after {waited:?}, not after two load timeouts"
            );
            // Each timed-out load is stopped, so nothing is left holding memory.
            assert_eq!(
                shepherd.calls(),
                vec![
                    Call::Restart("iq3_s".into()),
                    Call::Stop("iq3_s".into()),
                    Call::Restart("iq3_s".into()),
                    Call::Stop("iq3_s".into()),
                ]
            );
            let snapshot = engine.snapshot().await;
            assert_eq!(state_of(&engine, "iq3_s").await, Some(State::Unloaded));
            assert_eq!(snapshot.errors.len(), 1);
            assert_eq!(snapshot.errors[0].error, "not ready after 5m");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_load_for_a_name_not_in_the_config_fails_instead_of_panicking() {
    let mut engine = engine();
    let mut queue = VecDeque::new();

    engine.apply(vec![Action::Load("ghost".into())], &mut queue);

    assert!(engine.take_jobs().is_empty());
    assert!(matches!(
        queue.pop_front(),
        Some(Event::LoadFailed { model, error })
            if model == ModelName::from("ghost") && error == "no model named ghost in the config"
    ));
}
