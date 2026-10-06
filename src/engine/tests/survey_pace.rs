//! When the engine surveys the host, on a paused clock.

use super::{
    survey::{MIB, surveyed_every, view_of},
    *,
};
use crate::test_support::{FakeHost, captured::IDLE_TOTALS};

#[tokio::test(start_paused = true)]
async fn the_host_is_surveyed_every_thirty_seconds() {
    assert_eq!(SURVEY_EVERY, Duration::from_secs(30), "the spec's figure");
    let shepherd = FakeShepherd::new();
    let start = surveyed_every(FakeHost::absent(), SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            let _engine = engine;
            sleep(SURVEY_EVERY - SOON).await;
            assert_eq!(shepherd.describes(), 0, "none before the first period");
            sleep(2 * SOON).await;
            assert_eq!(shepherd.describes(), 1);
            sleep(SURVEY_EVERY).await;
            assert_eq!(shepherd.describes(), 2, "one per period, not back to back");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_survey_that_outlasts_its_period_is_not_started_again() {
    let shepherd = FakeShepherd::new();
    let (host, gate) = FakeHost::printing(IDLE_TOTALS, "").gated();
    let start = surveyed_every(host, SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            shepherd.set_memory("laya", 1_504 * MIB);

            sleep(3 * SURVEY_EVERY + SOON).await;
            assert_eq!(gate.asked(), 1, "the first survey is still under way");
            gate.open();
            until("the held survey's reading", || async {
                view_of(&engine, "laya")
                    .await
                    .is_some_and(|view| view.measured.ram.is_some())
            })
            .await;
        },
    )
    .await;
}

/// The first survey's `nvidia-smi` never finishes. It is dropped two periods in, and the next
/// survey starts at the period after.
#[tokio::test(start_paused = true)]
async fn a_survey_stalled_for_two_periods_is_dropped_and_surveys_resume() {
    let shepherd = FakeShepherd::new();
    let (host, gate) = FakeHost::printing(IDLE_TOTALS, "").gated();
    let start = surveyed_every(host, SURVEY_EVERY);
    with_engine_from(
        config(SHEEP_MODELS),
        shepherd.clone(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "laya").await);
            shepherd.set_memory("laya", 1_504 * MIB);

            sleep(4 * SURVEY_EVERY + SOON).await;
            assert_eq!(
                gate.asked(),
                2,
                "a new survey once the stalled one was dropped"
            );
            gate.open();
            until("the new survey's reading", || async {
                view_of(&engine, "laya")
                    .await
                    .is_some_and(|view| view.measured.ram.is_some())
            })
            .await;
        },
    )
    .await;
}
