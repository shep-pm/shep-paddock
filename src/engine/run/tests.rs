//! Backend jobs against the fake shepherd, on a paused clock.

use super::*;
use crate::test_support::{Call, FakeShepherd, config};

// Longer than any retry a job here makes.
const BOUND: Duration = Duration::from_secs(60);

const SHARED: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"

[models.laya-b]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;

/// laya's stop fails once and would try again five seconds later, after
/// laya-b's load has started the sheep.
#[tokio::test(start_paused = true)]
async fn a_load_replaces_a_stop_still_running_on_its_sheep() {
    let config = config(SHARED);
    let model = |name: &str| config.models[&ModelName::from(name)].clone();
    let shepherd = FakeShepherd::failing_stops(1);
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let mut jobs = Jobs::new(&backends);

    jobs.start(Job::Unload(model("laya")));
    let stopping = timeout(Duration::from_secs(1), jobs.next()).await;
    assert!(stopping.is_err(), "the stop finished: {stopping:?}");
    jobs.start(Job::Load(model("laya-b")));
    let first = timeout(BOUND, jobs.next()).await.expect("the load ends");
    let rest = timeout(BOUND, jobs.next()).await.expect("nothing runs on");

    assert!(
        matches!(&first, Some((name, Outcome::Loaded)) if *name == ModelName::from("laya-b")),
        "{first:?}"
    );
    assert!(rest.is_none(), "{rest:?}");
    assert_eq!(
        shepherd.calls(),
        [Call::Stop("laya".into()), Call::Restart("laya".into())]
    );
}
