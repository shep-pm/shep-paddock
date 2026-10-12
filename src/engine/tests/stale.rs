//! A loaded model whose launch settings changed, reloaded once nothing would be cut off.

use super::*;

/// [`SHEEP_MODELS`] with an env on laya's sheep.
fn laya_with_env() -> Arc<Config> {
    config(&SHEEP_MODELS.replace(
        r#"[models.laya]
backend = { sheep = "laya" }"#,
        r#"[models.laya]
backend = { sheep = "laya", env = { MODE = "b" } }"#,
    ))
}

// Past the default grace period of 120 seconds.
const PAST_GRACE: Duration = Duration::from_secs(130);

/// The request has finished, so nothing holds laya or uses it, and the reload waits only for the
/// grace period.
#[tokio::test(start_paused = true)]
async fn a_changed_env_on_a_loaded_model_is_applied_after_its_grace_period() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            engine.reconfigure(laya_with_env()).await;

            sleep(SOON).await;
            assert_eq!(
                shepherd.calls(),
                [Call::Restart("laya".into())],
                "laya reloaded inside its grace period"
            );
            assert!(
                engine
                    .snapshot()
                    .await
                    .models
                    .iter()
                    .any(|view| view.name == ModelName::from("laya") && view.stale),
                "laya was not stale"
            );

            sleep(PAST_GRACE).await;
            until("the reload", || async {
                calls_of(&shepherd, &Call::Restart("laya".into())) == 2
            })
            .await;

            assert_eq!(
                shepherd.calls(),
                [
                    Call::Restart("laya".into()),
                    Call::Stop("laya".into()),
                    Call::SetEnv("laya".into(), "MODE".into(), "b".into()),
                    Call::Restart("laya".into()),
                ]
            );
            until_state(&engine, "laya", State::Loaded).await;
            assert!(
                engine
                    .snapshot()
                    .await
                    .models
                    .iter()
                    .all(|view| !view.stale),
                "laya was still stale after its reload"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_reclaimable_lease_stream_does_not_end_across_the_reload() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(SHEEP_MODELS),
        shepherd.clone(),
        |engine| async move {
            let mut events = timeout(
                BOUND,
                engine.take_lease(
                    BENCH.into(),
                    LeaseRequest {
                        reclaimable: true,
                        ..lease_on("laya", Hold::Connection)
                    },
                ),
            )
            .await
            .expect("the engine took the ask");
            let _lease = granted(&mut events).await;
            engine.reconfigure(laya_with_env()).await;

            sleep(PAST_GRACE).await;
            until("the reload", || async {
                calls_of(&shepherd, &Call::Restart("laya".into())) == 2
            })
            .await;
            until_state(&engine, "laya", State::Loaded).await;

            assert!(
                timeout(SOON, events.recv()).await.is_err(),
                "the lease stream said something across the reload"
            );
            assert_eq!(engine.snapshot().await.leases.len(), 1);
        },
    )
    .await;
}
