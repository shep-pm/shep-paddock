//! The series built from snapshots, and the reporter on a paused clock beside a real engine.

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use shep_client::dogs::Stop;
use tokio::{
    task::{LocalSet, spawn_local},
    time::{sleep, timeout},
};

use super::{Channel, REPORT_EVERY, Sample, Sent, report, series};
use crate::{
    backend::Backends,
    book::{
        Hold, LeaseId, LeaseView, ModelView, Moment, Priority, Reason, Revocation, Snapshot, State,
        TurnHolder, TurnKind, WaiterKind, WaiterView,
    },
    config::{ClientName, ModelName},
    engine::{self, Start},
    footprint::{Footprint, Vram},
    survey::Measured,
    test_support::{FakeShepherd, HOST_AND_MODELS, config},
};

fn model(name: &str, state: State) -> ModelView {
    ModelView {
        name: ModelName::from(name),
        state,
        in_flight: 0,
        last_used: Moment(0),
        held_by: Vec::new(),
        unknown: false,
        stray: false,
        placement: None,
        footprint: Footprint {
            vram: Vram::None,
            ram: 0,
        },
        measured: Measured::default(),
        measured_peak: Measured::default(),
        drift: false,
        sequences: None,
        turn_ram: None,
        stale: false,
    }
}

fn lease(id: u64, model: Option<&str>) -> LeaseView {
    LeaseView {
        id: LeaseId(id),
        client: ClientName::from("bench-01"),
        model: model.map(ModelName::from),
        footprint: None,
        pid: None,
        priority: Priority::Batch,
        since: Moment(0),
        expected_until: None,
        note: None,
        hold: Hold::Connection,
        attached: true,
        reclaimable: false,
        last_activity: Moment(0),
        in_use: false,
        release_if_idle: None,
        revoked: None,
        measured: Measured::default(),
        drift: false,
    }
}

fn waiter(model: Option<&str>, reason: Option<Reason>) -> WaiterView {
    WaiterView {
        client: ClientName::from("mac-sessions"),
        model: model.map(ModelName::from),
        footprint: None,
        kind: WaiterKind::Request,
        lease: None,
        priority: Priority::Interactive,
        since: Moment(0),
        reason,
        estimate: None,
    }
}

fn loading(model: &str) -> Reason {
    Reason::Loading {
        model: ModelName::from(model),
    }
}

fn turn(model: &str) -> Reason {
    Reason::Turn {
        model: ModelName::from(model),
        turn: TurnKind::Request,
        holders: Vec::<TurnHolder>::new(),
        ahead: 0,
    }
}

fn snapshot(models: Vec<ModelView>, leases: Vec<LeaseView>, waiters: Vec<WaiterView>) -> Snapshot {
    Snapshot {
        models,
        leases,
        waiters,
        errors: Vec::new(),
        declared: Footprint {
            vram: Vram::None,
            ram: 0,
        },
        unaccounted_vram: None,
    }
}

/// The value of the sample named `name` whose labels are exactly `labels`
fn value(samples: &[Sample], name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    samples
        .iter()
        .find(|sample| {
            sample.series.name == name
                && sample.labels.len() == labels.len()
                && labels
                    .iter()
                    .all(|(key, want)| sample.labels.get(key).map(String::as_str) == Some(*want))
        })
        .map(|sample| sample.value)
}

fn first(snapshot: &Snapshot) -> Vec<Sample> {
    series(snapshot, &Sent::default()).0
}

#[test]
fn a_loaded_model_reports_1_and_every_other_state_0() {
    let states = [
        ("loaded", State::Loaded, 1.0),
        ("unloaded", State::Unloaded, 0.0),
        ("reserved", State::Reserved, 0.0),
        ("loading", State::Loading, 0.0),
        ("evicting", State::Evicting, 0.0),
        ("unloading", State::Unloading, 0.0),
    ];
    let models = states.iter().map(|(name, state, _)| model(name, *state));

    let samples = first(&snapshot(models.collect(), Vec::new(), Vec::new()));

    for (name, _, want) in states {
        assert_eq!(
            value(&samples, "paddock_model_loaded", &[("model", name)]),
            Some(want),
            "{name}"
        );
    }
}

#[test]
fn a_stray_model_is_reported_like_any_other() {
    let stray = ModelView {
        stray: true,
        unknown: true,
        ..model("sheep:iq3_s", State::Loaded)
    };

    let samples = first(&snapshot(vec![stray], Vec::new(), Vec::new()));

    assert_eq!(
        value(
            &samples,
            "paddock_model_loaded",
            &[("model", "sheep:iq3_s")]
        ),
        Some(1.0)
    );
}

#[test]
fn leases_are_counted_per_model_without_revoked_ones() {
    let revoked = LeaseView {
        revoked: Some(Revocation {
            by: ClientName::from("admin"),
            note: None,
        }),
        ..lease(4, Some("iq2_xs"))
    };
    let leases = vec![
        lease(1, Some("iq2_xs")),
        lease(2, Some("iq2_xs")),
        lease(3, Some("laya")),
        revoked,
    ];

    let samples = first(&snapshot(Vec::new(), leases, Vec::new()));

    assert_eq!(
        value(&samples, "paddock_leases", &[("model", "iq2_xs")]),
        Some(2.0)
    );
    assert_eq!(
        value(&samples, "paddock_leases", &[("model", "laya")]),
        Some(1.0)
    );
}

#[test]
fn a_bare_lease_and_a_bare_waiter_carry_no_model_label() {
    let leases = vec![lease(1, None), lease(2, None)];
    let waiters = vec![waiter(None, Some(loading("x")))];

    let samples = first(&snapshot(Vec::new(), leases, waiters));

    assert_eq!(value(&samples, "paddock_leases", &[]), Some(2.0));
    assert_eq!(
        value(&samples, "paddock_waiters", &[("reason_kind", "loading")]),
        Some(1.0)
    );
}

#[test]
fn waiters_are_counted_by_model_and_reason_kind_with_turn_among_them() {
    let waiters = vec![
        waiter(Some("iq2_xs"), Some(turn("iq2_xs"))),
        waiter(Some("iq2_xs"), Some(turn("iq2_xs"))),
        waiter(Some("iq2_xs"), Some(loading("iq2_xs"))),
        waiter(Some("laya"), Some(turn("laya"))),
    ];

    let samples = first(&snapshot(Vec::new(), Vec::new(), waiters));

    let count = |model, kind| {
        value(
            &samples,
            "paddock_waiters",
            &[("model", model), ("reason_kind", kind)],
        )
    };
    assert_eq!(count("iq2_xs", "turn"), Some(2.0));
    assert_eq!(count("iq2_xs", "loading"), Some(1.0));
    assert_eq!(count("laya", "turn"), Some(1.0));
}

#[test]
fn a_waiter_not_yet_told_a_reason_counts_under_none() {
    let waiters = vec![waiter(Some("iq2_xs"), None), waiter(Some("iq2_xs"), None)];

    let samples = first(&snapshot(Vec::new(), Vec::new(), waiters));

    assert_eq!(
        value(
            &samples,
            "paddock_waiters",
            &[("model", "iq2_xs"), ("reason_kind", "none")]
        ),
        Some(2.0)
    );
}

#[test]
fn a_label_set_that_empties_is_sent_once_at_zero_then_not_again() {
    let busy = snapshot(
        vec![model("iq2_xs", State::Loaded)],
        vec![lease(1, Some("iq2_xs"))],
        vec![waiter(Some("iq2_xs"), Some(turn("iq2_xs")))],
    );
    let quiet = snapshot(vec![model("iq2_xs", State::Loaded)], Vec::new(), Vec::new());
    let (_, sent) = series(&busy, &Sent::default());

    let (emptied, sent) = series(&quiet, &sent);
    let (after, _) = series(&quiet, &sent);

    assert_eq!(
        value(&emptied, "paddock_leases", &[("model", "iq2_xs")]),
        Some(0.0)
    );
    assert_eq!(
        value(
            &emptied,
            "paddock_waiters",
            &[("model", "iq2_xs"), ("reason_kind", "turn")]
        ),
        Some(0.0)
    );
    assert_eq!(
        value(&emptied, "paddock_model_loaded", &[("model", "iq2_xs")]),
        Some(1.0)
    );
    assert_eq!(after.len(), 1, "{after:?}");
}

#[test]
fn a_model_gone_from_the_book_sends_zero_once_for_each_of_its_series() {
    let there = snapshot(
        vec![model("iq2_xs", State::Loaded)],
        vec![lease(1, Some("iq2_xs"))],
        vec![waiter(Some("iq2_xs"), None)],
    );
    let gone = snapshot(Vec::new(), Vec::new(), Vec::new());
    let (_, sent) = series(&there, &Sent::default());

    let (zeroed, sent) = series(&gone, &sent);
    let (after, _) = series(&gone, &sent);

    assert_eq!(zeroed.len(), 3, "{zeroed:?}");
    assert!(zeroed.iter().all(|sample| sample.value == 0.0));
    assert!(after.is_empty(), "{after:?}");
}

#[test]
fn one_model_never_reports_more_than_ten_series() {
    let kinds = [
        None,
        Some(loading("m")),
        Some(Reason::Evicting {
            model: ModelName::from("m"),
            for_model: ModelName::from("other").into(),
        }),
        Some(Reason::Draining {
            model: ModelName::from("m"),
        }),
        Some(Reason::Grace {
            model: ModelName::from("m"),
            until: Moment(1),
        }),
        Some(Reason::Held {
            model: ModelName::from("other").into(),
            client: ClientName::from("bench-01"),
            lease: LeaseId(1),
            since: Moment(0),
            until: None,
            idle_since: None,
        }),
        Some(Reason::Behind {
            model: ModelName::from("other").into(),
        }),
        Some(turn("m")),
    ];
    let waiters = kinds.into_iter().map(|reason| waiter(Some("m"), reason));

    let samples = first(&snapshot(
        vec![model("m", State::Loaded)],
        vec![lease(1, Some("m"))],
        waiters.collect(),
    ));

    assert_eq!(samples.len(), 10, "{samples:?}");
}

#[test]
fn every_sample_is_a_gauge_with_help_and_a_name_the_metrics_dog_accepts() {
    let samples = first(&snapshot(
        vec![model("iq2_xs", State::Loaded)],
        vec![lease(1, Some("iq2_xs")), lease(2, None)],
        vec![waiter(Some("iq2_xs"), Some(turn("iq2_xs")))],
    ));
    let word = |text: &str, colon: bool| {
        let mut chars = text.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || (colon && c == ':'))
    };

    assert!(!samples.is_empty());
    for sample in &samples {
        let name = sample.series.name;
        assert!(name.starts_with("paddock_"), "{name}");
        assert!(!name.starts_with("shep_"), "{name}");
        assert!(word(name, true), "{name}");
        assert!(!sample.series.help.is_empty(), "{name}");
        for label in sample.labels.keys() {
            assert!(word(label, false), "{label}");
            assert!(
                !["sheep", "id", "fold", "dog", "source"].contains(label),
                "{label}"
            );
            assert!(!label.starts_with("__"), "{label}");
        }
    }
}

/// A channel that records what it is sent
struct Recorder {
    active: bool,
    sent: RefCell<Vec<Sample>>,
}

impl Recorder {
    fn new(active: bool) -> Self {
        Self {
            active,
            sent: RefCell::new(Vec::new()),
        }
    }

    fn count(&self) -> usize {
        self.sent.borrow().len()
    }
}

impl Channel for Recorder {
    fn is_active(&self) -> bool {
        self.active
    }

    fn send(&self, sample: &Sample) {
        self.sent.borrow_mut().push(sample.clone());
    }
}

/// Runs `body` beside a real engine holding the spec's models, all unloaded
async fn with_engine<F, Fut>(body: F)
where
    F: FnOnce(engine::EngineHandle) -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    let start = config(HOST_AND_MODELS);
    let (handle, inbox) = engine::channel();
    let backends = Backends::new(FakeShepherd::default(), crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(engine::run(
        Arc::clone(&start),
        backends,
        Start::default(),
        inbox,
        Stop::never(),
    ));
    local.run_until(body(handle)).await;
}

#[tokio::test(start_paused = true)]
async fn the_reporter_sends_every_fifteen_seconds() {
    assert_eq!(REPORT_EVERY, Duration::from_secs(15));
    with_engine(|handle| async move {
        let recorder = Rc::new(Recorder::new(true));
        let (stop, _request) = Stop::new();
        let sink = Rc::clone(&recorder);
        let reporting = spawn_local(async move { report(&*sink, &handle, stop).await });

        sleep(Duration::from_secs(1)).await;
        let one_round = recorder.count();
        assert!(one_round > 0, "nothing was sent at the start");

        sleep(REPORT_EVERY - Duration::from_secs(2)).await;
        assert_eq!(recorder.count(), one_round, "sent before the interval");
        sleep(Duration::from_secs(2)).await;
        assert_eq!(recorder.count(), 2 * one_round);
        sleep(REPORT_EVERY).await;
        assert_eq!(recorder.count(), 3 * one_round);
        reporting.abort();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_the_reporter_while_it_waits() {
    with_engine(|handle| async move {
        let recorder = Recorder::new(true);
        let (stop, request) = Stop::new();
        let reporting = spawn_local({
            let recorder = Rc::new(recorder);
            async move { report(&*recorder, &handle, stop).await }
        });
        sleep(Duration::from_secs(1)).await;

        request.request();

        let ended = timeout(Duration::from_secs(1), reporting).await;
        assert!(ended.is_ok(), "the reporter outlived the stop");
    })
    .await;
}

/// An engine nobody runs never answers a snapshot, so asking for one would hang.
#[tokio::test(start_paused = true)]
async fn a_dog_without_a_channel_never_asks_for_a_snapshot() {
    let (handle, _inbox) = engine::channel();
    let recorder = Recorder::new(false);
    let (stop, _request) = Stop::new();

    let returned = timeout(Duration::from_secs(60), report(&recorder, &handle, stop)).await;

    assert!(returned.is_ok(), "it waited on the engine");
    assert_eq!(recorder.count(), 0);
}
