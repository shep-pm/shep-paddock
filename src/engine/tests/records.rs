//! The record of what each sheep runs, which ends once nothing the dog loaded runs there.

use super::{
    restart::{read_state, state_in},
    *,
};

/// [`SHEEP_MODELS`] with laya moved to the laya-2 sheep.
fn laya_moved() -> Arc<Config> {
    config(&SHEEP_MODELS.replace(
        r#"backend = { sheep = "laya" }"#,
        r#"backend = { sheep = "laya-2" }"#,
    ))
}

/// [`SHEEP_MODELS`] with laya gone, and its sheep with it.
fn laya_removed() -> Arc<Config> {
    config(
        SHEEP_MODELS
            .split("[models.laya]")
            .next()
            .unwrap_or_default(),
    )
}

/// An engine writing `state.json` under `home`.
fn saving_engine(home: &std::path::Path) -> Engine {
    let mut engine = engine();
    engine.restore(Start {
        state: Some(state_in(home)),
        ..Start::default()
    });
    engine
}

fn reconfigure(engine: &mut Engine, config: Arc<Config>) {
    let (done, _) = tokio::sync::oneshot::channel();
    engine.command(crate::engine::Command::Reconfigure { config, done });
}

/// Asks for `model` and reports its load done.
fn load(engine: &mut Engine, waiter: u64, model: &str) {
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(waiter),
        client: MAC.into(),
        model: model.into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    let jobs = engine.take_jobs();
    assert!(
        matches!(jobs.as_slice(), [Job::Load(loading)] if loading.name == ModelName::from(model)),
        "{jobs:?}"
    );
    engine.finished(model.into(), Outcome::Loaded);
    assert_eq!(engine.book.state(&model.into()), Some(State::Loaded));
}

/// Asks for `model`, and reports its load and its one retry failed, which leaves its sheep to shep.
fn load_fails(engine: &mut Engine, waiter: u64, model: &str) {
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(waiter),
        client: MAC.into(),
        model: model.into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    for _ in 0..2 {
        let _ = engine.take_jobs();
        engine.finished(model.into(), Outcome::LoadFailed("exited".into()));
    }
    assert_eq!(engine.book.state(&model.into()), Some(State::Unloaded));
    assert!(engine.take_jobs().is_empty(), "nothing stops the sheep");
}

/// laya's load failed, so the dog never stopped its sheep. A reload then
/// moved laya to laya-2, where it loaded. Its old sheep going down is not
/// laya's crash.
#[tokio::test(start_paused = true)]
async fn the_sheep_a_model_moved_off_going_down_is_not_its_crash() {
    let mut engine = engine();
    load_fails(&mut engine, 1, "laya");
    reconfigure(&mut engine, laya_moved());
    load(&mut engine, 2, "laya");

    engine.process(crash("laya", ProcessKind::Exit, true), |_| false);

    assert_eq!(engine.book.state(&"laya".into()), Some(State::Loaded));
    assert_eq!(
        engine.expected_running().len(),
        1,
        "only laya-2 is expected"
    );
}

/// laya crashed, and the dog stopped its sheep to be sure.
#[tokio::test(start_paused = true)]
async fn a_sheep_the_dog_stopped_leaves_the_state_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let mut engine = saving_engine(home.path());
    load(&mut engine, 1, "laya");
    assert_eq!(
        read_state(&state_in(home.path())).sheep.get("laya"),
        Some(&ModelName::from("laya"))
    );
    engine.process(crash("laya", ProcessKind::Exit, false), |_| false);
    let jobs = engine.take_jobs();
    assert!(
        matches!(jobs.as_slice(), [Job::Unload(model)] if model.name == ModelName::from("laya")),
        "{jobs:?}"
    );

    engine.finished("laya".into(), Outcome::Unloaded);

    let saved = read_state(&state_in(home.path()));
    assert_eq!(saved.sheep.get("laya"), None, "{:?}", saved.sheep);
}

/// laya's load failed, so the dog never stopped its sheep. The reload that
/// drops the sheep from the config drops its record.
#[tokio::test(start_paused = true)]
async fn a_sheep_the_config_no_longer_names_leaves_the_state_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let mut engine = saving_engine(home.path());
    load_fails(&mut engine, 1, "laya");

    reconfigure(&mut engine, laya_removed());

    let saved = read_state(&state_in(home.path()));
    assert_eq!(saved.sheep.get("laya"), None, "{:?}", saved.sheep);
}

/// A request keeps laya loaded on the sheep the reload dropped, so its record stays until it stops.
#[tokio::test(start_paused = true)]
async fn a_dropped_sheep_still_running_its_model_keeps_its_record() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let mut engine = saving_engine(home.path());
    load(&mut engine, 1, "laya");

    reconfigure(&mut engine, laya_moved());

    assert_eq!(engine.book.state(&"laya".into()), Some(State::Loaded));
    let saved = read_state(&state_in(home.path()));
    assert_eq!(saved.sheep.get("laya"), Some(&ModelName::from("laya")));
}

/// laya moved off its sheep as in the first test here, and tagger took that sheep, so the config
/// still names it.
#[tokio::test(start_paused = true)]
async fn a_sheep_still_configured_that_a_model_moved_off_going_down_is_not_its_crash() {
    let mut engine = engine();
    load_fails(&mut engine, 1, "laya");
    let moved = SHEEP_MODELS.replace(
        r#"backend = { sheep = "laya" }"#,
        r#"backend = { sheep = "laya-2" }"#,
    );
    let tagger = r#"
[models.tagger]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8001"
ram = "1G"
idle = "8h"
"#;
    reconfigure(&mut engine, config(&format!("{moved}{tagger}")));
    load(&mut engine, 2, "laya");

    engine.process(crash("laya", ProcessKind::Exit, true), |_| false);

    assert_eq!(engine.book.state(&"laya".into()), Some(State::Loaded));
}
