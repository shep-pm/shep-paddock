//! When the engine surveys the host, on a paused clock.

use super::{survey::surveyed_every, *};
use crate::test_support::FakeHost;

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
