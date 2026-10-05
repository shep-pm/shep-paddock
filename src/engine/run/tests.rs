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

/// laya's load gives up on its second crash and laya-b's load on the same sheep
/// replaces laya's quiet stop. The shepherd refuses every restart, so laya-b
/// never starts and laya's process may still run: once laya-b's load is given
/// up on too, laya's stop runs after all.
#[tokio::test(start_paused = true)]
async fn a_failed_load_runs_the_quiet_stop_it_replaced() {
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = Engine::new(config(SHARED), super::super::Clock::new(), notify);
    let shepherd = FakeShepherd::refusing_restart("laya: restart refused");
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let mut jobs = Jobs::new(&backends);
    for (waiter, model) in [(1, "laya"), (2, "laya-b")] {
        engine.feed(Event::RequestArrived {
            waiter: crate::book::WaiterId(waiter),
            client: "mac-sessions".into(),
            model: model.into(),
            priority: crate::book::Priority::Interactive,
            max_wait: BOUND,
        });
    }
    let _ = engine.take_jobs();
    let exited = || ProcessEvent {
        sheep: "laya".to_owned(),
        kind: crate::shepherd::ProcessKind::Exit,
        manually: false,
    };
    engine.process(exited());
    let _ = engine.take_jobs();
    engine.process(exited());

    // laya-b's load, then the book's one retry of it, then whatever follows.
    for _ in 0..3 {
        for job in engine.take_jobs() {
            jobs.start(job);
        }
        let Some((model, outcome)) = timeout(BOUND, jobs.next()).await.expect("a job ends") else {
            break;
        };
        engine.finished(model, outcome);
    }

    assert_eq!(
        engine.book.state(&ModelName::from("laya-b")),
        Some(crate::book::State::Unloaded)
    );
    assert_eq!(
        shepherd.calls(),
        [
            Call::Restart("laya".into()),
            Call::Restart("laya".into()),
            Call::Stop("laya".into()),
        ]
    );
    assert!(engine.take_jobs().is_empty());
}
