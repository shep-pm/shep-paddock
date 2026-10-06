//! Strays: sheep that come online without the dog.

use super::*;
use crate::book::ModelView;

async fn view_of(engine: &EngineHandle, model: &str) -> Option<ModelView> {
    let model = ModelName::from(model);
    engine
        .snapshot()
        .await
        .models
        .into_iter()
        .find(|view| view.name == model)
}

async fn until_stray(engine: &EngineHandle, model: &str) {
    until(&format!("{model} counted as a loaded stray"), || async {
        view_of(engine, model)
            .await
            .is_some_and(|view| view.state == State::Loaded && view.stray)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_sheep_that_comes_online_by_hand_is_its_one_model_as_a_stray() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            feed.send(online("laya")).expect("the engine subscribed");
            until_stray(&engine, "laya").await;

            drop(forwarded(&engine, "laya").await);
            assert_eq!(
                shepherd.calls(),
                vec![],
                "served by the stray, not restarted"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_shared_sheep_that_comes_online_by_hand_is_an_evictable_stand_in() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            feed.send(online("iq2_xs")).expect("the engine subscribed");
            until_stray(&engine, "sheep:iq2_xs").await;
            assert!(
                view_of(&engine, "sheep:iq2_xs")
                    .await
                    .is_some_and(|view| view.unknown)
            );

            drop(forwarded(&engine, "iq3_s").await);
            assert_eq!(
                shepherd.calls(),
                vec![Call::Stop("iq2_xs".into()), Call::Restart("iq3_s".into())]
            );
            assert_eq!(
                state_of(&engine, "sheep:iq2_xs").await,
                None,
                "a stand-in is forgotten once unloaded"
            );
        },
    )
    .await;
}

/// Events are read in order, so laya's stray is seen only after postgres was read.
#[tokio::test(start_paused = true)]
async fn a_sheep_no_model_names_is_not_counted() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            feed.send(online("postgres"))
                .expect("the engine subscribed");
            feed.send(online("laya")).expect("the engine subscribed");
            until_stray(&engine, "laya").await;
            let names: Vec<_> = engine
                .snapshot()
                .await
                .models
                .into_iter()
                .map(|view| view.name)
                .collect();
            assert_eq!(
                names.len(),
                4,
                "the four configured models and nothing else: {names:?}"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_online_from_the_dogs_own_load_is_not_a_stray() {
    let shepherd = FakeShepherd::new().gated();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let admitting = spawn_local(admit(engine.clone(), "laya"));
            until_called(&shepherd, Call::Restart("laya".into())).await;
            feed.send(online("laya")).expect("the engine subscribed");
            sleep(SOON).await;
            shepherd.open_gate();

            let admitted = timeout(BOUND, admitting).await;
            assert!(
                matches!(admitted, Ok(Ok(Admission::Forward(_)))),
                "{admitted:?}"
            );
            assert!(
                view_of(&engine, "laya")
                    .await
                    .is_some_and(|view| !view.stray)
            );
        },
    )
    .await;
}

/// The first stop of iq2_xs fails and is tried again five seconds later. An `online` in
/// between is the sheep the dog is still stopping.
#[tokio::test(start_paused = true)]
async fn an_online_while_the_dog_stops_the_sheep_is_not_a_stray() {
    let shepherd = FakeShepherd::failing_stops(1);
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            let admitting = spawn_local(admit(engine.clone(), "iq3_s"));
            until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
            feed.send(online("iq2_xs")).expect("the engine subscribed");

            let admitted = timeout(BOUND, admitting).await;
            assert!(
                matches!(admitted, Ok(Ok(Admission::Forward(_)))),
                "{admitted:?}"
            );
            let models = engine.snapshot().await.models;
            assert!(models.iter().all(|view| !view.stray), "{models:?}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stray_sheep_that_exits_is_forgotten_and_not_stopped() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            feed.send(online("laya")).expect("the engine subscribed");
            until_stray(&engine, "laya").await;
            feed.send(crash("laya", ProcessKind::Exit, false))
                .expect("the engine subscribed");
            until_state(&engine, "laya", State::Unloaded).await;
            assert_eq!(stops(&shepherd), 0);
        },
    )
    .await;
}

/// Both sheep crashed while the dog was down, so discovery counts neither,
/// and shep restarts them once the dog is up.
#[tokio::test(start_paused = true)]
async fn a_sheep_down_at_start_that_comes_online_is_counted_as_a_stray() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.crash("laya");
    shepherd.crash("iq2_xs");
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let saved = crate::saved::Saved::default();
    let discovered = timeout(BOUND, crate::discover::discover(&config, &backends, &saved))
        .await
        .expect("discovery finishes");
    assert_eq!(discovered.loaded, vec![], "nothing counts at start");
    let start = Start {
        state: None,
        saved,
        discovered,
    };
    let feed = shepherd.feed();
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        feed.send(online("laya")).expect("the engine subscribed");
        feed.send(online("iq2_xs")).expect("the engine subscribed");
        until_stray(&engine, "laya").await;
        until_stray(&engine, "sheep:iq2_xs").await;
        assert_eq!(shepherd.calls(), vec![]);
    })
    .await;
}

/// laya's `idle` is 8h, and its stray is counted as used when it is found.
#[tokio::test(start_paused = true)]
async fn a_stray_that_idles_out_is_unloaded_by_stopping_its_sheep() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            feed.send(online("laya")).expect("the engine subscribed");
            until_stray(&engine, "laya").await;

            sleep(Duration::from_secs(8 * 3600) - SOON).await;
            assert_eq!(state_of(&engine, "laya").await, Some(State::Loaded));
            sleep(SOON * 2).await;
            until_state(&engine, "laya", State::Unloaded).await;
            assert_eq!(shepherd.calls(), vec![Call::Stop("laya".into())]);
            assert!(
                view_of(&engine, "laya")
                    .await
                    .is_some_and(|view| !view.stray)
            );
        },
    )
    .await;
}

/// No stand-in for the sheep was counted, and the sheep still serves
/// iq2_xs-256k: its exit is read as that model's and stops the sheep.
async fn still_serving_256k(engine: &EngineHandle, shepherd: &FakeShepherd, feed: &Feed) {
    let models = engine.snapshot().await.models;
    assert!(
        models
            .iter()
            .all(|view| !view.stray && view.name != ModelName::from("sheep:iq2_xs")),
        "{models:?}"
    );
    assert_eq!(state_of(engine, "iq2_xs-256k").await, Some(State::Loaded));
    feed.send(crash("iq2_xs", ProcessKind::Exit, false))
        .expect("the engine subscribed");
    until_called(shepherd, Call::Stop("iq2_xs".into())).await;
    until_state(engine, "iq2_xs-256k", State::Unloaded).await;
}

type Feed = tokio::sync::mpsc::UnboundedSender<ProcessEvent>;

#[tokio::test(start_paused = true)]
async fn an_online_for_a_shared_sheep_the_dog_is_loading_is_not_a_stray() {
    let shepherd = FakeShepherd::new().gated();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let admitting = spawn_local(admit(engine.clone(), "iq2_xs-256k"));
            until_called(&shepherd, Call::Restart("iq2_xs".into())).await;
            feed.send(online("iq2_xs")).expect("the engine subscribed");
            sleep(SOON).await;
            shepherd.open_gate();

            let admitted = timeout(BOUND, admitting).await;
            assert!(
                matches!(admitted, Ok(Ok(Admission::Forward(_)))),
                "{admitted:?}"
            );
            still_serving_256k(&engine, &shepherd, &feed).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_online_for_a_shared_sheep_the_dog_is_running_is_not_a_stray() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.feed();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs-256k").await);
            feed.send(online("iq2_xs")).expect("the engine subscribed");
            sleep(SOON).await;

            still_serving_256k(&engine, &shepherd, &feed).await;
        },
    )
    .await;
}
