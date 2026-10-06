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

/// An ollama stand-in that accepts connections and never answers, and how many it took.
async fn silent_ollama() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    (url, accepted)
}

fn ollama_model(url: &str) -> Model {
    let mut model = crate::test_support::model("laya");
    model.backend = Backend::Ollama {
        url: url.to_owned(),
        name: "qwen".to_owned(),
    };
    model
}

/// Waits, on the paused clock, until `accepted` reaches `want` or the budget runs out.
async fn until_accepted(accepted: &std::sync::atomic::AtomicUsize, want: usize) -> usize {
    for _ in 0..10_000 {
        let seen = accepted.load(std::sync::atomic::Ordering::SeqCst);
        if seen >= want {
            return seen;
        }
        sleep(Duration::from_millis(10)).await;
    }
    accepted.load(std::sync::atomic::Ordering::SeqCst)
}

#[tokio::test(start_paused = true)]
async fn an_unload_that_is_never_answered_is_tried_again() {
    let (url, accepted) = silent_ollama().await;
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let mut jobs = Jobs::new(&backends);

    jobs.start(Job::Unload(ollama_model(&url)));
    let waiting = timeout(UNLOAD_ATTEMPT * 3, jobs.next()).await;

    assert!(waiting.is_err(), "the unload ended: {waiting:?}");
    assert!(until_accepted(&accepted, 2).await >= 2, "no second attempt");
}

#[tokio::test(start_paused = true)]
async fn a_cleanup_stop_that_is_never_answered_gives_up_on_the_load() {
    let (url, _accepted) = silent_ollama().await;
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let mut jobs = Jobs::new(&backends);

    jobs.start(Job::Cleanup(ollama_model(&url), "timed out".to_owned()));
    let ended = timeout(UNLOAD_ATTEMPT * 2, jobs.next())
        .await
        .expect("the cleanup ends");

    assert!(
        matches!(&ended, Some((_, Outcome::LoadFailed(error))) if error == "timed out"),
        "{ended:?}"
    );
}
