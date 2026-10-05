//! A model removed from the config while it holds its sheep, and a new model on that sheep.

use super::*;
use crate::book::Refusal;

/// [`SHEEP_MODELS`] with laya gone and laya-b added on laya's sheep.
fn laya_replaced() -> Arc<Config> {
    let without_laya = SHEEP_MODELS
        .split("[models.laya]")
        .next()
        .unwrap_or_default();
    config(&format!(
        r#"{without_laya}
[models.laya-b]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#
    ))
}

/// The lease keeps laya on its sheep after the reload, so laya-b may not
/// restart that sheep under it.
#[tokio::test(start_paused = true)]
async fn a_new_model_on_a_removed_held_models_sheep_is_refused_as_held() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let _lease = granted(&mut events).await;
            engine.reconfigure(laya_replaced()).await;

            let admitted = timeout(SOON, admit(engine.clone(), "laya-b"))
                .await
                .expect("answered");

            let Admission::Refused(Refusal {
                reason: Reason::Held { model, .. },
                ..
            }) = &admitted
            else {
                panic!("laya-b was not refused as held: {admitted:?}");
            };
            assert_eq!(*model, ModelName::from("laya"));
            assert_eq!(state_of(&engine, "laya").await, Some(State::Loaded));
            assert_eq!(shepherd.calls(), [Call::Restart("laya".into())]);
        },
    )
    .await;
}

/// laya's stop fails once and is tried again five seconds later. laya-b
/// waits for it, and laya leaves the book once it is down.
#[tokio::test(start_paused = true)]
async fn a_new_model_waits_for_a_removed_model_unloading_from_its_sheep() {
    let shepherd = FakeShepherd::failing_stops(1);
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            engine.reconfigure(laya_replaced()).await;
            until_called(&shepherd, Call::Stop("laya".into())).await;

            let waiting = spawn_local(admit(engine.clone(), "laya-b"));
            sleep(SOON).await;
            assert_eq!(state_of(&engine, "laya-b").await, Some(State::Reserved));
            let admitted = timeout(BOUND, waiting)
                .await
                .expect("answered")
                .expect("the request ran");

            assert!(matches!(admitted, Admission::Forward(_)), "{admitted:?}");
            assert_eq!(state_of(&engine, "laya").await, None, "laya was left");
            assert_eq!(
                shepherd.calls(),
                [
                    Call::Restart("laya".into()),
                    Call::Stop("laya".into()),
                    Call::Stop("laya".into()),
                    Call::Restart("laya".into()),
                ]
            );
        },
    )
    .await;
}
