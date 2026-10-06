//! The watcher against a fake shepherd on a paused clock, beside a real engine.

use std::{future::Future, sync::Arc, time::Duration};

use shep_client::dogs::Stop;
use tokio::{
    sync::watch,
    task::{LocalSet, spawn_local},
    time::{sleep, timeout},
};

use super::{RESUBSCRIBE_DELAY, ReloadError, reload, watch as follow};
use crate::{
    backend::Backends,
    book::Priority,
    config::Config,
    engine::{Admission, EngineHandle, Start, channel, run},
    test_support::{FakeShepherd, HOST_AND_MODELS, config},
};

// Longer than anything the watcher waits for, a resubscribe delay included.
const BOUND: Duration = Duration::from_secs(60);
const DROPPED: &str = "iq2_xs-256k";

/// The spec's section without one model nothing else refers to.
fn without_the_dropped_model() -> String {
    let from = HOST_AND_MODELS.find("[models.iq2_xs-256k]").expect("model");
    let to = HOST_AND_MODELS.find("[models.iq3_s]").expect("next model");
    format!("{}{}", &HOST_AND_MODELS[..from], &HOST_AND_MODELS[to..])
}

struct Rig {
    engine: EngineHandle,
    applied: watch::Receiver<Arc<Config>>,
}

async fn until<F: FnMut() -> bool>(what: &str, mut done: F) {
    let waited = timeout(BOUND, async {
        while !done() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what} did not happen within {BOUND:?}");
}

/// Runs `body` beside an engine and a watcher that both start from the spec's section.
async fn with_watcher<F, Fut>(shepherd: FakeShepherd, body: F)
where
    F: FnOnce(Rig) -> Fut,
    Fut: Future<Output = ()>,
{
    let start = config(HOST_AND_MODELS);
    let (handle, inbox) = channel();
    let (sender, applied) = watch::channel(Arc::clone(&start));
    let (stop, _request) = Stop::new();
    let local = LocalSet::new();
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    local.spawn_local(run(
        Arc::clone(&start),
        backends,
        Start::default(),
        inbox,
        Stop::never(),
    ));
    let rig = Rig {
        engine: handle.clone(),
        applied,
    };
    local
        .run_until(async move {
            let watching = spawn_local(async move {
                follow(
                    &shepherd,
                    "paddock",
                    HOST_AND_MODELS.to_owned(),
                    &handle,
                    &sender,
                    stop,
                )
                .await;
            });
            body(rig).await;
            watching.abort();
        })
        .await;
}

impl Rig {
    fn has_model(&self) -> bool {
        self.applied.borrow().models.contains_key(&DROPPED.into())
    }

    async fn applied_without_the_model(&self) {
        until("the new config reaching the endpoint", || !self.has_model()).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_valid_change_reaches_the_engine_and_then_the_endpoint() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.config_feed();
    with_watcher(shepherd.clone(), |rig| async move {
        assert!(rig.has_model(), "the model is configured before the change");
        until("the first read", || shepherd.section_reads() >= 1).await;
        shepherd.set_section(&without_the_dropped_model());
        feed.send(()).expect("the watcher listens");
        rig.applied_without_the_model().await;
        let asking = rig.engine.admit(
            "mac-sessions".into(),
            DROPPED.into(),
            Priority::Interactive,
            BOUND,
        );
        let asked = timeout(BOUND, asking).await.expect("the engine answers");
        assert!(
            matches!(asked, Admission::Unknown),
            "the engine no longer knows the model: {asked:?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_invalid_section_keeps_the_old_config_and_the_next_valid_one_applies() {
    let shepherd = FakeShepherd::new();
    let feed = shepherd.config_feed();
    with_watcher(shepherd.clone(), |rig| async move {
        until("the first read", || shepherd.section_reads() >= 1).await;
        shepherd.set_section("listen = \"not an address\"");
        feed.send(()).expect("the watcher listens");
        until("the invalid section being read", || {
            shepherd.section_reads() >= 2
        })
        .await;
        // Past the read, the watcher validates and finds no change to apply.
        sleep(Duration::from_secs(1)).await;
        assert!(rig.has_model(), "the old config is kept");
        assert!(!rig.applied.has_changed().expect("sender lives"));
        shepherd.set_section(&without_the_dropped_model());
        feed.send(()).expect("the watcher still listens");
        rig.applied_without_the_model().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_section_equal_to_the_running_one_is_not_applied_again() {
    let shepherd = FakeShepherd::new();
    shepherd.set_section(HOST_AND_MODELS);
    let feed = shepherd.config_feed();
    with_watcher(shepherd.clone(), |rig| async move {
        until("the first read", || shepherd.section_reads() >= 1).await;
        feed.send(()).expect("the watcher listens");
        until("the section being read again", || {
            shepherd.section_reads() >= 2
        })
        .await;
        sleep(Duration::from_secs(1)).await;
        assert!(!rig.applied.has_changed().expect("sender lives"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_change_made_while_the_stream_was_down_is_applied_on_the_next_subscription() {
    let shepherd = FakeShepherd::new();
    let first = shepherd.config_feed();
    shepherd.set_section(&without_the_dropped_model());
    with_watcher(shepherd.clone(), |rig| async move {
        until("the first subscription", || {
            shepherd.config_subscriptions() == 1
        })
        .await;
        drop(first);
        until("the second subscription", || {
            shepherd.config_subscriptions() == 2
        })
        .await;
        rig.applied_without_the_model().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_subscription_is_asked_again_only_after_the_delay() {
    let shepherd = FakeShepherd::new();
    shepherd.refuse_config_subscription();
    with_watcher(shepherd.clone(), |_rig| async move {
        until("the first subscription", || {
            shepherd.config_subscriptions() == 1
        })
        .await;
        sleep(RESUBSCRIBE_DELAY - Duration::from_millis(100)).await;
        assert_eq!(shepherd.config_subscriptions(), 1, "not asked again early");
        until("the second subscription", || {
            shepherd.config_subscriptions() == 2
        })
        .await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_the_watcher() {
    let shepherd = FakeShepherd::new();
    let (stop, request) = Stop::new();
    let (handle, _inbox) = channel();
    let (sender, _applied) = watch::channel(config(HOST_AND_MODELS));
    let local = LocalSet::new();
    let ended = local
        .run_until(async {
            request.request();
            timeout(
                BOUND,
                follow(&shepherd, "paddock", String::new(), &handle, &sender, stop),
            )
            .await
        })
        .await;
    assert!(
        ended.is_ok(),
        "the watcher returns once a stop is requested"
    );
}

#[test]
fn an_invalid_section_is_named_without_its_keys() {
    let text = "listen = \"nonsense\"\n[host]\nvram = \"1G\"\nram = \"1G\"\n\
                [[clients]]\nname = \"a\"\nkey = \"s3cret-key\"\n";
    let Err(err) = Config::from_toml(text) else {
        panic!("the section is invalid");
    };
    let shown = ReloadError::Invalid(err).to_string();
    assert!(shown.contains("keeping the old config"), "{shown}");
    assert!(shown.contains("listen"), "{shown}");
    assert!(!shown.contains("s3cret-key"), "{shown}");
}

#[test]
fn a_read_failure_says_it_kept_the_old_config() {
    let shown = ReloadError::Read(crate::shepherd::ShepherdError::Unexpected { what: "Pong" });
    assert_eq!(
        shown.to_string(),
        "reading the section failed, keeping the old config: shepherd answered with an unexpected Pong"
    );
}

/// Reloads `section` over the spec's section, and returns the notice and the config left running.
async fn reload_once(section: &str) -> (Option<String>, Arc<Config>) {
    let shepherd = FakeShepherd::new();
    shepherd.set_section(section);
    let start = config(HOST_AND_MODELS);
    let (handle, inbox) = channel();
    let (sender, applied) = watch::channel(Arc::clone(&start));
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(run(start, backends, Start::default(), inbox, Stop::never()));
    let mut last = HOST_AND_MODELS.to_owned();
    let notice = local
        .run_until(timeout(
            BOUND,
            reload(&shepherd, "paddock", &mut last, &handle, &sender),
        ))
        .await
        .expect("the reload finishes")
        .expect("the section is valid");
    let running = Arc::clone(&applied.borrow());
    (notice.map(|notice| notice.to_string()), running)
}

#[tokio::test(start_paused = true)]
async fn a_changed_listen_is_reported_as_needing_a_restart_and_not_applied() {
    let moved = HOST_AND_MODELS.replace("0.0.0.0:8700", "127.0.0.1:9999");
    let (notice, running) = reload_once(&moved).await;
    assert_eq!(
        notice.as_deref(),
        Some(
            "listen changed from 0.0.0.0:8700 to 127.0.0.1:9999, which needs a restart; still listening on 0.0.0.0:8700"
        )
    );
    assert_eq!(running.listen, "0.0.0.0:8700".parse().unwrap());
}

#[tokio::test(start_paused = true)]
async fn a_reload_that_leaves_listen_alone_says_nothing() {
    let changed = HOST_AND_MODELS.replace("idle = \"2h\"", "idle = \"3h\"");
    assert_ne!(changed, HOST_AND_MODELS);
    let (notice, _running) = reload_once(&changed).await;
    assert_eq!(notice, None);
}
