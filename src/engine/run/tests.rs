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

#[tokio::test(start_paused = true)]
async fn a_cleanup_stop_that_fails_is_tried_again() {
    let config = config(SHARED);
    let laya = config.models[&ModelName::from("laya")].clone();
    let shepherd = FakeShepherd::failing_stops(1);
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let mut jobs = Jobs::new(&backends);

    jobs.start(Job::Cleanup(laya, "timed out".to_owned()));
    let ended = timeout(BOUND, jobs.next()).await.expect("the cleanup ends");

    assert!(
        matches!(&ended, Some((_, Outcome::LoadFailed(error))) if error == "timed out"),
        "{ended:?}"
    );
    assert_eq!(
        shepherd.calls(),
        [Call::Stop("laya".into()), Call::Stop("laya".into())]
    );
}

/// laya has no ready check, so only its sheep coming online loads it.
#[tokio::test(start_paused = true)]
async fn a_sheep_that_never_comes_online_times_its_load_out() {
    let config = config(SHARED);
    let mut laya = config.models[&ModelName::from("laya")].clone();
    laya.load_timeout = Duration::from_secs(30);
    let shepherd = FakeShepherd::starting_restart();
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());

    let outcome = timeout(BOUND, load(&backends, laya))
        .await
        .expect("the load ends");

    assert!(
        matches!(outcome, Outcome::TimedOut(after) if after == Duration::from_secs(30)),
        "{outcome:?}"
    );
    assert_eq!(shepherd.calls(), [Call::Restart("laya".into())]);
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
        pid: None,
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

/// An ollama stand-in that accepts connections and never answers, and word of each it took.
async fn silent_ollama() -> (String, tokio::sync::mpsc::UnboundedReceiver<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (took, accepted) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
            let _ = took.send(());
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

// Real time: the silent ollama is a real loopback socket, so the pace is short.
#[tokio::test]
async fn an_unload_that_is_never_answered_is_tried_again() {
    let (url, mut accepted) = silent_ollama().await;
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let pace = UnloadPace {
        attempt: Duration::from_millis(200),
        retry: Duration::from_millis(50),
    };
    let mut jobs = Jobs::paced(&backends, pace);

    jobs.start(Job::Unload(ollama_model(&url)));
    let two_attempts = async {
        accepted.recv().await;
        accepted.recv().await;
    };
    let raced = timeout(Duration::from_secs(10), async {
        tokio::select! {
            ended = jobs.next() => Err(ended),
            () = two_attempts => Ok(()),
        }
    })
    .await;

    assert!(matches!(raced, Ok(Ok(()))), "{raced:?}");
}
