//! The watcher against a fake shepherd on a paused clock, beside a real engine, and against a
//! real client on a fake daemon for the reads a shepherd resolves.

use std::{future::Future, sync::Arc, time::Duration};

use shep_client::{
    ReconnectingClient,
    dogs::Stop,
    shep_core::protocol::{BusEvent, Request, RpcError, RpcErrorCode},
    testing::{Handovers, Handshake, control_address, fake_daemon_across_handovers, sample_ack},
};
use tokio::{
    sync::watch,
    task::{LocalSet, spawn_local},
    time::{sleep, timeout},
};

use super::{POLL_EVERY, RESUBSCRIBE_DELAY, ReloadError, reload, watch as follow};
use crate::{
    backend::Backends,
    book::Priority,
    config::Config,
    engine::{Admission, EngineHandle, Start, channel, run},
    shepherd::Live,
    test_support::{FakeShepherd, HOST_AND_MODELS, config, with_client_key},
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
async fn a_section_changed_with_no_notice_is_applied_on_the_next_poll() {
    let shepherd = FakeShepherd::new();
    let _feed = shepherd.config_feed();
    with_watcher(shepherd.clone(), |rig| async move {
        until("the first read", || shepherd.section_reads() >= 1).await;
        // An edit made outside shep: the section changes and nothing is sent on the feed.
        shepherd.set_section(&without_the_dropped_model());
        rig.applied_without_the_model().await;
        assert_eq!(shepherd.config_subscriptions(), 1, "no resubscribing");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_poll_of_an_unchanged_section_applies_nothing() {
    let shepherd = FakeShepherd::new();
    shepherd.set_section(HOST_AND_MODELS);
    let _feed = shepherd.config_feed();
    with_watcher(shepherd.clone(), |rig| async move {
        until("the first read", || shepherd.section_reads() >= 1).await;
        let first = shepherd.section_reads();
        sleep(POLL_EVERY * 3).await;
        assert!(shepherd.section_reads() > first, "the section was polled");
        assert!(!rig.applied.has_changed().expect("sender lives"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_invalid_section_found_by_a_poll_is_logged_once() {
    // The watcher logs each `Err` that `reload` returns, so one `Err` per text is one line.
    let shepherd = FakeShepherd::new();
    shepherd.set_section("listen = \"not an address\"");
    let start = config(HOST_AND_MODELS);
    let (handle, inbox) = channel();
    let (sender, applied) = watch::channel(Arc::clone(&start));
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(run(start, backends, Start::default(), inbox, Stop::never()));
    let mut last = HOST_AND_MODELS.to_owned();
    let polls = local
        .run_until(async {
            let mut polls = Vec::new();
            for _ in 0..3 {
                polls.push(reload(&shepherd, "paddock", &mut last, &handle, &sender).await);
            }
            polls
        })
        .await;
    assert!(
        matches!(polls[0], Err(ReloadError::Invalid(_))),
        "the first poll judges the text: {:?}",
        polls[0]
    );
    assert!(
        matches!(polls[1], Ok(None)) && matches!(polls[2], Ok(None)),
        "later polls of the same text say nothing: {polls:?}"
    );
    assert!(!applied.has_changed().expect("sender lives"));
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_the_watcher_while_it_waits_for_the_next_poll() {
    let shepherd = FakeShepherd::new();
    let _feed = shepherd.config_feed();
    let (stop, request) = Stop::new();
    let (handle, _inbox) = channel();
    let (sender, _applied) = watch::channel(config(HOST_AND_MODELS));
    let local = LocalSet::new();
    let ended = local
        .run_until(async {
            let watching = spawn_local({
                let shepherd = shepherd.clone();
                async move {
                    follow(
                        &shepherd,
                        "paddock",
                        HOST_AND_MODELS.to_owned(),
                        &handle,
                        &sender,
                        stop,
                    )
                    .await;
                }
            });
            until("the first read", || shepherd.section_reads() >= 1).await;
            request.request();
            timeout(Duration::from_secs(1), watching).await
        })
        .await;
    let joined = ended.expect("the watcher returns before the next poll");
    joined.expect("the watcher task ends cleanly");
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

/// A real client on a fake shepherd that answers the dog-section requests, beside a real
/// engine and a watcher following the section.
///
/// The fake answers every read with whatever was last armed, which stands for a shepherd that
/// hands a dog its section with each `{{secret:...}}` replaced by the value it holds for it.
struct LiveRig {
    applied: watch::Receiver<Arc<Config>>,
    daemon: Handovers,
}

impl LiveRig {
    /// Arms the section the next read is answered with.
    fn answer(&self, section: &str) {
        self.daemon.reply_to_dog_config(section);
    }

    /// Tells the dog its own section changed, as the shepherd does for a write of its own.
    async fn announce(&self) {
        self.daemon
            .push(BusEvent::DogConfigChanged {
                dog: "paddock".to_owned(),
            })
            .await;
    }

    /// Every read of the section the fake was asked for, in order.
    fn section_reads(&self) -> Vec<Request> {
        self.daemon
            .envelopes()
            .into_iter()
            .map(|(_, envelope)| envelope.body)
            .filter(|body| {
                matches!(
                    body,
                    Request::DogConfig { .. } | Request::DogConfigResolved { .. }
                )
            })
            .collect()
    }

    /// Whether the running config takes `key` for the client named `client`.
    fn accepts(&self, client: &str, key: &str) -> bool {
        self.applied
            .borrow()
            .clients
            .iter()
            .any(|c| c.name.as_str() == client && c.key_matches(key.as_bytes()))
    }
}

/// Runs `body` beside an engine and a watcher, both over a real client on a fake shepherd that
/// answers the first read with `first`.
async fn with_live_watcher<F, Fut>(first: &str, body: F)
where
    F: FnOnce(LiveRig) -> Fut,
    Fut: Future<Output = ()>,
{
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = control_address(dir.path());
    let daemon = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
    daemon.reply_to_dog_config(first);
    let client = timeout(
        BOUND,
        ReconnectingClient::connect_as_dog(&socket, "paddock"),
    )
    .await
    .expect("the dog connects")
    .expect("the shepherd answers the handshake");
    let live = Live::new(client);

    let start = config(first);
    let (handle, inbox) = channel();
    let (sender, applied) = watch::channel(Arc::clone(&start));
    let backends = Backends::new(live.clone(), crate::outbound::http_client());
    let (stop, _request) = Stop::new();
    let local = LocalSet::new();
    local.spawn_local(run(
        Arc::clone(&start),
        backends,
        Start::default(),
        inbox,
        Stop::never(),
    ));
    let rig = LiveRig { applied, daemon };
    local
        .run_until(async move {
            let watching = spawn_local({
                let live = live.clone();
                let first = first.to_owned();
                async move {
                    follow(&live, "paddock", first, &handle, &sender, stop).await;
                }
            });
            body(rig).await;
            watching.abort();
        })
        .await;
}

/// The acceptance criterion's seam: a `{{secret:...}}` written in the `[paddock]` section
/// reaches the dog as the value the shepherd holds for it, at the first read and again after a
/// config change.
#[tokio::test]
async fn a_secret_in_the_section_applies_as_the_value_the_shepherd_holds() {
    const WRITTEN: &str = "{{secret:mac-key}}";
    const HELD: &str = "value-the-shepherd-holds-for-mac-key";
    const ROTATED: &str = "the-value-after-the-rotation";
    let first = with_client_key(HOST_AND_MODELS, HELD);
    let next = with_client_key(HOST_AND_MODELS, ROTATED);
    assert_ne!(first, HOST_AND_MODELS, "the section under test has a key");

    with_live_watcher(&first, |rig| async move {
        until("the first read of the section", || {
            !rig.section_reads().is_empty()
        })
        .await;
        assert!(rig.accepts("mac-sessions", HELD), "the start config");
        assert!(!rig.accepts("mac-sessions", WRITTEN));

        rig.answer(&next);
        rig.announce().await;
        until("the rotated value reaching the endpoint", || {
            rig.accepts("mac-sessions", ROTATED)
        })
        .await;

        let reads = rig.section_reads();
        assert!(reads.len() >= 2, "the section was read again: {reads:?}");
        assert!(
            reads
                .iter()
                .all(|read| matches!(read, Request::DogConfigResolved { .. })),
            "only the resolved request gets a section with its references replaced: {reads:?}"
        );
    })
    .await;
}

/// An old shepherd answers the plain request alone, and its section still names a reference.
/// Handing that to the dog would make the reference itself the key, so the read fails and the
/// running config is kept.
#[tokio::test]
async fn a_section_the_shepherd_cannot_resolve_keeps_the_running_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = control_address(dir.path());
    let daemon = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
    daemon.reply_to_dog_config(&with_client_key(HOST_AND_MODELS, "{{secret:mac-key}}"));
    daemon.refuse_dog_config_resolved(RpcError {
        code: RpcErrorCode::Unsupported,
        message: "this shepherd does not implement the request".to_owned(),
        daemon_version: None,
    });
    let client = timeout(
        BOUND,
        ReconnectingClient::connect_as_dog(&socket, "paddock"),
    )
    .await
    .expect("the dog connects")
    .expect("the shepherd answers the handshake");
    let live = Live::new(client);
    let (handle, _inbox) = channel();
    let (sender, applied) = watch::channel(config(HOST_AND_MODELS));

    let mut last = String::new();
    let read = reload(&live, "paddock", &mut last, &handle, &sender).await;

    let Err(err) = read else {
        panic!("a section that still holds a reference is not applied: {read:?}");
    };
    assert!(matches!(err, ReloadError::Read(_)), "{err}");
    let shown = err.to_string();
    assert!(!shown.contains("{{secret:"), "{shown}");
    assert!(
        applied
            .borrow()
            .clients
            .iter()
            .any(|c| c.name.as_str() == "mac-sessions" && c.key_matches(b"k-mac")),
        "the running config is left as it was"
    );
    drop(daemon);
    drop(dir);
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
