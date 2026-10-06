//! Starting from saved leases and discovered models, and writing `state.json`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::{
    book::Found,
    config::Model,
    discover::{Discovered, discover, stand_in},
    saved::{self, Saved, SavedHold, SavedLease, SavedModel},
};

/// The saved state a restart reads: `sheep` as `(sheep, model)` pairs, and `leases`.
pub(super) fn saved_with(sheep: &[(&str, &str)], leases: Vec<SavedLease>) -> Saved {
    Saved {
        leases,
        sheep: sheep
            .iter()
            .map(|(sheep, model)| ((*sheep).to_owned(), ModelName::from(*model)))
            .collect(),
        ..Saved::default()
    }
}

/// What discovery reports with `models` loaded by the dog and `sheep` running unknown.
fn found(config: &Config, models: &[&str], sheep: &[&str]) -> Discovered {
    let counted = |model: &Model, stray| Found {
        model: model.name.clone(),
        footprint: model.footprint,
        placement: None,
        stray,
    };
    let stand_ins: Vec<_> = sheep
        .iter()
        .map(|sheep| stand_in(config, sheep).expect("a model runs on the sheep"))
        .collect();
    let loaded = models
        .iter()
        .map(|name| counted(&config.models[&ModelName::from(*name)], false))
        .chain(stand_ins.iter().map(|model| counted(model, true)))
        .collect();
    Discovered {
        loaded,
        stand_ins,
        ..Discovered::default()
    }
}

/// A wall-clock time `hours` from now, in whole milliseconds as the engine's clock keeps it.
fn hours_from_now(hours: i64) -> Timestamp {
    let now = Timestamp::now() + SignedDuration::from_hours(hours);
    Timestamp::from_millisecond(now.as_millisecond()).expect("in range")
}

pub(super) fn bench_lease(id: u64, model: &str, hold: SavedHold) -> SavedLease {
    SavedLease {
        id: LeaseId(id),
        client: BENCH.into(),
        model: model.into(),
        priority: Priority::Batch,
        since: hours_from_now(-30 * 24),
        expected_until: Some(hours_from_now(8)),
        note: Some("strata h2h run 3".to_owned()),
        hold,
        last_activity: None,
        release_if_idle_ms: None,
        reclaimable: false,
    }
}

pub(super) fn state_in(home: &Path) -> PathBuf {
    saved::path_in(home)
}

pub(super) fn read_state(path: &Path) -> Saved {
    saved::load(path)
        .expect("the state file reads")
        .expect("the state file exists")
}

#[tokio::test(start_paused = true)]
async fn a_restored_models_crash_is_noticed() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");
    let feed = shepherd.feed();
    let start = Start {
        saved: saved_with(&[("iq2_xs", "iq2_xs")], Vec::new()),
        discovered: found(&config, &["iq2_xs"], &[]),
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        assert_eq!(state_of(&engine, "iq2_xs").await, Some(State::Loaded));

        feed.send(crash("iq2_xs", ProcessKind::Exit, false))
            .expect("the engine subscribed");
        until_called(&shepherd, Call::Stop("iq2_xs".into())).await;
        until_state(&engine, "iq2_xs", State::Unloaded).await;
    })
    .await;
}

/// The crash came between discovery and the engine's first listing of the flock.
#[tokio::test(start_paused = true)]
async fn a_restored_models_missed_crash_is_found_in_the_flock() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.crash("iq3_s");
    let start = Start {
        saved: saved_with(&[("iq3_s", "iq3_s")], Vec::new()),
        discovered: found(&config, &["iq3_s"], &[]),
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        until_called(&shepherd, Call::Stop("iq3_s".into())).await;
        until_state(&engine, "iq3_s", State::Unloaded).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unknown_sheep_is_never_served_and_is_stopped_for_room() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");
    let start = Start {
        discovered: found(&config, &[], &["iq2_xs"]),
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        let snapshot = engine.snapshot().await;
        let unknown = snapshot
            .models
            .iter()
            .find(|view| view.name == ModelName::from("sheep:iq2_xs"))
            .expect("the unknown sheep is in the book");
        assert!(unknown.unknown);
        assert_eq!(unknown.state, State::Loaded);
        assert!(matches!(
            timeout(SOON, admit(engine.clone(), "sheep:iq2_xs")).await,
            Ok(Admission::Unknown)
        ));

        drop(forwarded(&engine, "iq3_s").await);

        assert_eq!(
            shepherd.calls(),
            [Call::Stop("iq2_xs".into()), Call::Restart("iq3_s".into())]
        );
        assert_eq!(state_of(&engine, "sheep:iq2_xs").await, None);
    })
    .await;
}

/// The dog's own sheep crashed while the dog was down. It counts until the first listing finds
/// it not up. Then it is stopped, so shep's pending restart cannot bring it back uncounted.
#[tokio::test(start_paused = true)]
async fn a_sheep_waiting_to_restart_is_counted_then_stopped() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.waiting_restart("iq3_s");
    let mut saved = saved_with(&[("iq3_s", "iq3_s")], Vec::new());
    let dogs = SavedModel {
        placement: None,
        stray: false,
    };
    saved.models.insert(ModelName::from("iq3_s"), dogs);
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(BOUND, discover(&config, &backends, &saved))
        .await
        .expect("discovery finishes");
    let counted: Vec<_> = discovered
        .loaded
        .iter()
        .map(|found| (&found.model, found.stray))
        .collect();
    assert_eq!(counted, [(&ModelName::from("iq3_s"), false)]);
    let start = Start {
        saved,
        discovered,
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        until_called(&shepherd, Call::Stop("iq3_s".into())).await;
        until_state(&engine, "iq3_s", State::Unloaded).await;
    })
    .await;
}

/// A stand-in with no record is a stray. A stray whose sheep is not up went away by itself,
/// so it is forgotten with no stop.
#[tokio::test(start_paused = true)]
async fn a_stand_in_waiting_to_restart_is_forgotten_unstopped() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.waiting_restart("iq2_xs");
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(BOUND, discover(&config, &backends, &Saved::default()))
        .await
        .expect("discovery finishes");
    let stand_ins: Vec<_> = discovered.stand_ins.iter().map(|m| &m.name).collect();
    assert_eq!(stand_ins, [&ModelName::from("sheep:iq2_xs")]);
    let start = Start {
        discovered,
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        until("the stand-in leaving the book", || async {
            state_of(&engine, "sheep:iq2_xs").await.is_none()
        })
        .await;
        assert!(shepherd.calls().is_empty(), "{:?}", shepherd.calls());
    })
    .await;
}

/// A benchmark's lease outlives the dog: same id, same grant time, and its holder attaches again.
#[tokio::test(start_paused = true)]
async fn a_restored_lease_keeps_its_id_its_times_and_its_model() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.running("iq2_xs");
    let lease = bench_lease(5, "iq2_xs", SavedHold::Connection {});
    let start = Start {
        saved: saved_with(&[("iq2_xs", "iq2_xs")], vec![lease.clone()]),
        discovered: found(&config, &["iq2_xs"], &[]),
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        let clock = engine.clock();
        let view = engine.snapshot().await.leases.remove(0);
        assert_eq!(view.id, LeaseId(5));
        assert_eq!(clock.wall(view.since), lease.since);
        assert_eq!(
            view.expected_until.map(|at| clock.wall(at)),
            lease.expected_until
        );
        assert!(!view.attached, "no stream survives a restart");

        let mut events = timeout(BOUND, engine.attach(BENCH.into(), LeaseId(5)))
            .await
            .expect("answered")
            .expect("its holder attaches");
        assert_eq!(granted(&mut events).await, LeaseId(5));
        sleep(Duration::from_secs(600)).await;

        assert_eq!(engine.snapshot().await.leases.len(), 1);
        assert_eq!(state_of(&engine, "iq2_xs").await, Some(State::Loaded));
        assert!(shepherd.calls().is_empty(), "the held model was touched");
        let mut next = engine
            .take_lease(MAC.into(), lease_on("iq2_xs", Hold::Connection))
            .await;
        assert_eq!(granted(&mut next).await, LeaseId(6));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_restored_lease_whose_model_is_not_loaded_loads_it() {
    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    let hold = SavedHold::Heartbeat { ttl_ms: 600_000 };
    let start = Start {
        saved: saved_with(&[], vec![bench_lease(2, "laya", hold)]),
        ..Start::default()
    };
    with_engine_from(config, shepherd.clone(), start, |engine| async move {
        until_called(&shepherd, Call::Restart("laya".into())).await;
        until_state(&engine, "laya", State::Loaded).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn leases_survive_a_restart_through_the_state_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let first = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        first,
        |engine| async move {
            let ttl = Duration::from_secs(600);
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Heartbeat { ttl }))
                .await;
            granted(&mut events).await;
        },
    )
    .await;
    let saved = read_state(&path);
    assert_eq!(saved.leases.len(), 1);

    let config = config(SHEEP_MODELS);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");
    let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
    let discovered = timeout(BOUND, discover(&config, &backends, &saved))
        .await
        .expect("discovery finishes");
    let second = Start {
        state: Some(path.clone()),
        saved: saved.clone(),
        discovered,
    };
    with_engine_from(config, shepherd.clone(), second, |engine| async move {
        let view = engine.snapshot().await.leases.remove(0);
        assert_eq!(view.id, saved.leases[0].id);
        assert_eq!(engine.clock().wall(view.since), saved.leases[0].since);
        assert_eq!(state_of(&engine, "laya").await, Some(State::Loaded));
        assert_eq!(engine.renew(BENCH.into(), view.id).await, Ok(()));
        assert!(shepherd.calls().is_empty(), "laya was loaded again");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_load_records_its_sheep_and_model_in_the_state_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let start = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    let shepherd = FakeShepherd::new();
    with_engine_from(config(SHEEP_MODELS), shepherd, start, |engine| async move {
        let only = |model: &str| {
            let unplaced = SavedModel {
                placement: None,
                stray: false,
            };
            BTreeMap::from([(ModelName::from(model), unplaced)])
        };
        drop(forwarded(&engine, "iq2_xs-256k").await);
        let saved = read_state(&path);
        assert_eq!(
            saved.sheep.get("iq2_xs"),
            Some(&ModelName::from("iq2_xs-256k"))
        );
        assert_eq!(saved.models, only("iq2_xs-256k"));

        drop(forwarded(&engine, "iq2_xs").await);
        let saved = read_state(&path);
        assert_eq!(saved.sheep.get("iq2_xs"), Some(&ModelName::from("iq2_xs")));
        assert_eq!(saved.models, only("iq2_xs"), "the unloaded model left");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_released_lease_leaves_the_state_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let start = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        start,
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;
            assert_eq!(read_state(&path).leases.len(), 1);

            assert_eq!(engine.release(BENCH.into(), lease).await, Ok(()));

            assert!(read_state(&path).leases.is_empty());
        },
    )
    .await;
}

/// IR-41: a client's key and a model's key stay out of `state.json`.
#[tokio::test(start_paused = true)]
async fn the_state_file_holds_no_key() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let keyed = config(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "bench-01"
key = "k-bench-secret"

[models.laya]
backend = { sheep = "laya", env = { TOKEN = "env-secret" } }
url = "http://127.0.0.1:8000"
key = "k-laya-secret"
ram = "5G"
idle = "8h"
"#,
    );
    let start = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    with_engine_from(keyed, FakeShepherd::new(), start, |engine| async move {
        let mut events = engine
            .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
            .await;
        granted(&mut events).await;
    })
    .await;

    let text = std::fs::read_to_string(&path).expect("the state file exists");
    assert!(text.contains("bench-01"), "{text}");
    for secret in ["k-bench-secret", "k-laya-secret", "env-secret"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_state_file_that_cannot_be_written_does_not_stop_leases() {
    let home = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        home.path().join("paddock"),
        "a file where the directory goes",
    )
    .expect("written");
    let start = Start {
        state: Some(state_in(home.path())),
        ..Start::default()
    };
    with_engine_from(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        start,
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;

            assert_eq!(engine.release(BENCH.into(), lease).await, Ok(()));
        },
    )
    .await;
}
