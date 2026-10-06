//! Each model holding memory reaching `state.json`, with its placement and stray flag.

use super::{
    restart::{read_state, state_in},
    *,
};
use crate::saved;

/// Loading qwen on ollama starts no sheep, so no sheep's save covers it.
#[tokio::test(start_paused = true)]
async fn an_ollama_load_names_its_model_in_state_json() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(
        config(crate::test_support::HOST_AND_MODELS),
        Clock::new(),
        notify,
    );
    engine.restore(Start {
        state: Some(path.clone()),
        ..Start::default()
    });

    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "qwen3.8:27b".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });

    let saved = saved::load(&path).expect("readable").unwrap_or_default();
    let names: Vec<_> = saved.models.keys().map(ModelName::as_str).collect();
    assert_eq!(names, ["qwen3.8:27b"]);
}

#[tokio::test(start_paused = true)]
async fn a_model_unloaded_for_idleness_leaves_state_json() {
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
            drop(forwarded(&engine, "laya").await);
            assert_eq!(read_state(&path).models.len(), 1);

            // Past laya's 8h idle time, and the fake's stop.
            sleep(Duration::from_secs(8 * 3600 + 60)).await;
            assert_eq!(state_of(&engine, "laya").await, Some(State::Unloaded));

            assert!(read_state(&path).models.is_empty());
        },
    )
    .await;
}

/// Something other than the dog started laya's sheep.
#[tokio::test(start_paused = true)]
async fn the_state_file_names_a_stray_as_one() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let config = config(SHEEP_MODELS);
    let laya = config.models[&ModelName::from("laya")].clone();
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(config, Clock::new(), notify);
    engine.restore(Start {
        state: Some(path.clone()),
        ..Start::default()
    });

    engine.feed(Event::StrayFound {
        model: laya.name,
        footprint: laya.footprint,
        backend: laya.backend,
    });

    let models = read_state(&path).models;
    let stray = models.get(&ModelName::from("laya")).map(|kept| kept.stray);
    assert_eq!(stray, Some(true));
}

/// iq2_xs leaves for qwen, and qwen loads only once a reload frees it of VRAM.
#[tokio::test(start_paused = true)]
async fn a_load_a_reload_starts_names_its_model_in_state_json() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = state_in(home.path());
    let shared = crate::test_support::HOST_AND_MODELS;
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(config(shared), Clock::new(), notify);
    let iq2_xs = ModelName::from("iq2_xs");
    let footprint = config(shared).models[&iq2_xs].footprint;
    engine.restore(Start {
        state: Some(path.clone()),
        discovered: crate::discover::Discovered {
            loaded: vec![(iq2_xs, footprint)],
            ..crate::discover::Discovered::default()
        },
        ..Start::default()
    });
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: "qwen3.8:27b".into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    assert_eq!(
        engine.book.state(&"qwen3.8:27b".into()),
        Some(State::Reserved)
    );

    let (done, _) = tokio::sync::oneshot::channel();
    let in_ram = shared.replacen("vram = \"22323M\"\n", "", 1);
    engine.command(crate::engine::Command::Reconfigure {
        config: config(&in_ram),
        done,
    });

    assert_eq!(
        engine.book.state(&"qwen3.8:27b".into()),
        Some(State::Loading)
    );
    let names: Vec<_> = read_state(&path).models.into_keys().collect();
    assert!(names.contains(&ModelName::from("qwen3.8:27b")), "{names:?}");
}
