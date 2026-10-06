//! A placement's script and env reach the shepherd before the restart.

use serde_json::json;

use super::*;
use crate::config::PlacementName;

/// iq2_xs takes the whole card, and laya has the spec's two placements. No ready checks, so
/// a load ends when the fake's restart answers.
const PLACED_SHEEP: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"

[models.iq2_xs]
backend = { sheep = "iq2_xs" }
url = "http://127.0.0.1:8080"
vram = "all"
ram = "37G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
idle = "8h"

[[models.laya.placements]]
name = "gpu"
vram = "6G"
ram = "2G"
script = "/opt/laya/venv-gpu/bin/laya-serve"
env = { LAYA_DEVICE = "cuda", CUDA_VISIBLE_DEVICES = "0" }

[[models.laya.placements]]
name = "ram"
ram = "5G"
script = "/opt/laya/venv/bin/laya-serve"
env = { LAYA_DEVICE = "cpu", CUDA_VISIBLE_DEVICES = "" }
"#;

async fn placement_of(engine: &EngineHandle, model: &str) -> Option<PlacementName> {
    let snapshot = engine.snapshot().await;
    let model = ModelName::from(model);
    snapshot
        .models
        .into_iter()
        .find(|view| view.name == model)?
        .placement
}

#[tokio::test(start_paused = true)]
async fn laya_starts_on_the_gpu_with_that_placements_script_and_env() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(PLACED_SHEEP),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "laya").await);

            assert_eq!(
                shepherd.calls(),
                vec![
                    Call::SetEnv("laya".into(), "CUDA_VISIBLE_DEVICES".into(), "0".into()),
                    Call::SetEnv("laya".into(), "LAYA_DEVICE".into(), "cuda".into()),
                    Call::SetField(
                        "laya".into(),
                        "script".into(),
                        json!("/opt/laya/venv-gpu/bin/laya-serve")
                    ),
                    Call::Restart("laya".into()),
                ]
            );
            assert_eq!(
                placement_of(&engine, "laya").await,
                Some(PlacementName::from("gpu"))
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn laya_starts_in_ram_while_strata_holds_the_card() {
    let shepherd = FakeShepherd::new();
    with_engine(
        config(PLACED_SHEEP),
        shepherd.clone(),
        |engine| async move {
            drop(forwarded(&engine, "iq2_xs").await);
            drop(forwarded(&engine, "laya").await);

            assert_eq!(
                shepherd.calls(),
                vec![
                    Call::Restart("iq2_xs".into()),
                    Call::SetEnv("laya".into(), "CUDA_VISIBLE_DEVICES".into(), String::new()),
                    Call::SetEnv("laya".into(), "LAYA_DEVICE".into(), "cpu".into()),
                    Call::SetField(
                        "laya".into(),
                        "script".into(),
                        json!("/opt/laya/venv/bin/laya-serve")
                    ),
                    Call::Restart("laya".into()),
                ]
            );
            assert_eq!(state_of(&engine, "iq2_xs").await, Some(State::Loaded));
            assert_eq!(
                placement_of(&engine, "laya").await,
                Some(PlacementName::from("ram"))
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_state_file_names_the_placement_laya_loaded_in() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = super::restart::state_in(home.path());
    let start = Start {
        state: Some(path.clone()),
        ..Start::default()
    };
    with_engine_from(
        config(PLACED_SHEEP),
        FakeShepherd::new(),
        start,
        |engine| async move {
            drop(forwarded(&engine, "laya").await);

            let models = super::restart::read_state(&path).models;
            let laya = models.get(&ModelName::from("laya")).expect("laya is named");
            assert_eq!(laya.placement, Some(PlacementName::from("gpu")));
            assert!(!laya.stray);
        },
    )
    .await;
}
