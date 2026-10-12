//! A model's `turn_ram`, counted in its footprint once per turn.

use super::*;
use crate::footprint::Vram;

const GIB: u64 = 1 << 30;

/// The maintainer's iq3_xxs on a host big enough for it, with `extra` after its own keys.
fn iq3_xxs(extra: &str) -> String {
    format!(
        r#"
[host]
vram = "24G"
ram = "62G"

[models.iq3_xxs]
backend = {{ sheep = "strata" }}
url = "http://127.0.0.1:8080"
vram = "all"
ram = "41G"
idle = "2h"
{extra}
"#
    )
}

#[test]
fn turn_ram_adds_sequences_plus_one_turns_to_the_ram_footprint() {
    let config = Config::from_toml(&iq3_xxs("sequences = 2\nturn_ram = \"4G\"")).unwrap();
    let model = &config.models[&name("iq3_xxs")];
    assert_eq!(model.turn_ram, Some(4 * GIB));
    assert_eq!(model.footprint.ram, 53 * GIB);
    assert_eq!(model.footprint.vram, Vram::All);
}

/// A model with placements on the same host, with `extra` after its own keys.
fn placed(extra: &str, gpu_ram: &str, cpu_ram: &str) -> String {
    format!(
        r#"
[host]
vram = "24G"
ram = "62G"

[models.laya]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
idle = "8h"
{extra}
[[models.laya.placements]]
name = "gpu"
vram = "6G"
ram = "{gpu_ram}"

[[models.laya.placements]]
name = "ram"
ram = "{cpu_ram}"
"#
    )
}

#[test]
fn turn_ram_is_added_to_each_placements_ram() {
    let text = placed("sequences = 1\nturn_ram = \"3G\"", "2G", "5G");
    let config = Config::from_toml(&text).unwrap();
    let laya = &config.models[&name("laya")];
    let ram = |placement: &str| {
        laya.placements
            .iter()
            .find(|p| p.name.as_str() == placement)
            .map(|p| p.footprint.ram)
    };
    // Two turns of 3G: one for the lease and one for the request turn
    assert_eq!(ram("gpu"), Some(8 * GIB));
    assert_eq!(ram("ram"), Some(11 * GIB));
    assert_eq!(laya.footprint.ram, 11 * GIB);
    assert_eq!(laya.footprint.vram, Vram::Bytes(6 * GIB));
}

#[test]
fn a_placement_that_fits_only_without_its_turns_is_refused() {
    // 40G fits a 62G host alone, and 40G + 3 x 8G does not
    let text = placed("sequences = 2\nturn_ram = \"8G\"", "2G", "40G");
    assert_eq!(
        Config::from_toml(&text).map(|_| ()),
        Err(ConfigError::PlacementNeverFits {
            model: name("laya"),
            placement: PlacementName::from("ram"),
        })
    );
    assert!(Config::from_toml(&placed("", "2G", "40G")).is_ok());
}

#[test]
fn a_model_that_fits_only_without_its_turns_never_fits() {
    // 41G fits a 62G host alone, and 41G + 3 x 8G does not
    let text = iq3_xxs("sequences = 2\nturn_ram = \"8G\"");
    assert_eq!(
        Config::from_toml(&text).map(|_| ()),
        Err(ConfigError::NeverFits {
            model: name("iq3_xxs")
        })
    );
    assert!(Config::from_toml(&iq3_xxs("sequences = 2")).is_ok());
}

#[test]
fn the_turns_sum_cannot_overflow_into_a_fit() {
    // 2^62 bytes in each of 4 turns is 2^64, which wraps to nothing
    let text = iq3_xxs("sequences = 3\nturn_ram = \"4294967296G\"");
    assert_eq!(
        Config::from_toml(&text).map(|_| ()),
        Err(ConfigError::NeverFits {
            model: name("iq3_xxs")
        })
    );
    let text = placed("sequences = 3\nturn_ram = \"4294967296G\"", "2G", "5G");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::PlacementNeverFits { .. })
    ));
}

#[test]
fn turn_ram_without_sequences_is_refused() {
    assert_eq!(
        Config::from_toml(&iq3_xxs("turn_ram = \"4G\"")).map(|_| ()),
        Err(ConfigError::TurnRamWithoutSequences {
            model: name("iq3_xxs")
        })
    );
}

#[test]
fn turn_ram_on_an_ollama_model_is_refused() {
    let text = two_ollama_models("qwen3", "ollama")
        .replace("[models.q]", "[models.q]\nsequences = 1\nturn_ram = \"1G\"");
    assert_eq!(
        Config::from_toml(&text).map(|_| ()),
        Err(ConfigError::TurnRamOnOllama { model: name("q") })
    );
}

#[test]
fn a_turn_ram_outside_shep_grammar_is_refused() {
    let Err(ConfigError::Size { field, .. }) =
        Config::from_toml(&iq3_xxs("sequences = 2\nturn_ram = \"4GB\""))
    else {
        panic!("4GB is not a size");
    };
    assert_eq!(field, "models.iq3_xxs.turn_ram");
}

#[test]
fn the_schema_accepts_a_size_for_turn_ram() {
    let schema = shep_client::dogs::config_schema::<section::Section>();
    let reference = schema
        .as_value()
        .pointer("/$defs/ModelSection/properties/turn_ram/anyOf/0/$ref")
        .and_then(serde_json::Value::as_str);
    assert_eq!(reference, Some("#/$defs/MemSize"));
}

#[test]
fn a_models_debug_shows_its_turn_ram() {
    let config = Config::from_toml(&iq3_xxs("sequences = 2\nturn_ram = \"4G\"")).unwrap();
    let shown = format!("{:?}", config.models[&name("iq3_xxs")]);
    assert!(shown.contains("turn_ram: Some(4294967296)"), "{shown}");
}
