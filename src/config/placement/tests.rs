use std::collections::BTreeMap;

use super::*;
use crate::{
    config::{Config, ConfigError},
    footprint::Vram,
    test_support::{LAYA_PLACEMENTS, placed, placed_toml},
};

const GIB: u64 = 1 << 30;

fn p(name: &str) -> PlacementName {
    PlacementName::from(name)
}

fn laya(config: &Config) -> &Model {
    &config.models[&ModelName::from("laya")]
}

/// One sheep model on a small host, with `placements` as the body after its own keys.
fn one_model(own: &str, placements: &str) -> Result<Config, ConfigError> {
    Config::from_toml(&format!(
        r#"
[host]
vram = "24G"
ram = "62G"

[models.laya]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
{own}idle = "8h"
{placements}"#
    ))
}

const TWO: &str = r#"
[[models.laya.placements]]
name = "gpu"
vram = "6G"
ram = "2G"

[[models.laya.placements]]
name = "ram"
ram = "5G"
"#;

#[test]
fn the_spec_example_reads_two_placements_in_order() {
    let config = placed();
    let laya = laya(&config);
    let names: Vec<_> = laya
        .placements
        .iter()
        .map(|placement| placement.name.as_str())
        .collect();
    assert_eq!(names, ["gpu", "ram"]);
    let gpu = &laya.placements[0];
    assert_eq!(
        gpu.footprint,
        Footprint {
            vram: Vram::Bytes(6 * GIB),
            ram: 2 * GIB
        }
    );
    assert_eq!(
        gpu.script.as_deref(),
        Some("/opt/laya/venv-gpu/bin/laya-serve")
    );
    assert_eq!(
        gpu.env,
        BTreeMap::from([
            ("CUDA_VISIBLE_DEVICES".to_owned(), "0".to_owned()),
            ("LAYA_DEVICE".to_owned(), "cuda".to_owned()),
        ])
    );
    assert_eq!(
        laya.placements[1].footprint,
        Footprint {
            vram: Vram::None,
            ram: 5 * GIB
        }
    );
}

#[test]
fn a_model_with_placements_counts_at_its_largest_of_each_resource() {
    let config = placed();
    assert_eq!(
        laya(&config).footprint,
        Footprint {
            vram: Vram::Bytes(6 * GIB),
            ram: 5 * GIB
        }
    );
}

#[test]
fn footprint_at_picks_the_placement_or_the_largest() {
    let config = placed();
    let laya = laya(&config);
    assert_eq!(
        laya.footprint_at(Some(&p("ram"))),
        Footprint {
            vram: Vram::None,
            ram: 5 * GIB
        }
    );
    assert_eq!(laya.footprint_at(None), laya.footprint);
    assert_eq!(
        laya.footprint_at(Some(&p("cpu"))),
        laya.footprint,
        "a placement it does not have"
    );
}

#[test]
fn placed_sets_the_placements_script_args_and_env_over_the_backends() {
    let toml = placed_toml().replace(
        "backend = { sheep = \"laya\" }",
        "backend = { sheep = \"laya\", args = [\"--port\", \"8000\"], env = { LAYA_DEVICE = \"auto\", LOG = \"info\" } }",
    );
    let config = crate::test_support::config(&toml);
    let placed = laya(&config).placed(&p("ram"));
    let Backend::Sheep {
        script, args, env, ..
    } = &placed.backend
    else {
        panic!("laya is on a sheep");
    };
    assert_eq!(script.as_deref(), Some("/opt/laya/venv/bin/laya-serve"));
    assert_eq!(
        args.as_deref(),
        Some(&["--port".to_owned(), "8000".to_owned()][..]),
        "the placement sets no args"
    );
    assert_eq!(
        env,
        &BTreeMap::from([
            ("CUDA_VISIBLE_DEVICES".to_owned(), String::new()),
            ("LAYA_DEVICE".to_owned(), "cpu".to_owned()),
            ("LOG".to_owned(), "info".to_owned()),
        ])
    );
    assert_eq!(
        placed.footprint,
        Footprint {
            vram: Vram::None,
            ram: 5 * GIB
        }
    );
}

#[test]
fn a_model_with_placements_and_its_own_figures_is_refused() {
    for own in ["vram = \"6G\"\n", "ram = \"5G\"\n"] {
        assert_eq!(
            one_model(own, TWO).map(|_| ()),
            Err(ConfigError::FootprintBesidePlacements {
                model: ModelName::from("laya")
            }),
            "{own}"
        );
    }
}

#[test]
fn placements_on_an_ollama_model_are_refused() {
    let toml = r#"
[host]
vram = "24G"
ram = "62G"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.qwen]
backend = "ollama"
name = "qwen3.8:27b"
idle = "2h"

[[models.qwen.placements]]
name = "gpu"
vram = "22G"
"#;
    assert_eq!(
        Config::from_toml(toml).map(|_| ()),
        Err(ConfigError::PlacementsOnOllama {
            model: ModelName::from("qwen")
        })
    );
}

#[test]
fn two_placements_with_one_name_are_refused() {
    let twice = TWO.replace("name = \"ram\"", "name = \"gpu\"");
    assert_eq!(
        one_model("", &twice).map(|_| ()),
        Err(ConfigError::DuplicatePlacement {
            model: ModelName::from("laya"),
            placement: p("gpu")
        })
    );
}

#[test]
fn placements_that_set_different_fields_are_refused() {
    let differ = |extra_on_gpu: &str| {
        let body = TWO.replace("vram = \"6G\"\n", &format!("vram = \"6G\"\n{extra_on_gpu}"));
        one_model("", &body).map(|_| ())
    };
    let refused = |what: &str| {
        Err(ConfigError::PlacementKeysDiffer {
            model: ModelName::from("laya"),
            first: p("gpu"),
            second: p("ram"),
            what: what.to_owned(),
        })
    };
    assert_eq!(differ("script = \"/opt/gpu\"\n"), refused("script"));
    assert_eq!(differ("args = [\"--cuda\"]\n"), refused("args"));
    assert_eq!(
        differ("env = { LAYA_DEVICE = \"cuda\" }\n"),
        refused("env key LAYA_DEVICE")
    );
}

#[test]
fn a_placement_that_can_never_fit_is_refused_by_name() {
    let big = TWO.replace("ram = \"5G\"", "ram = \"63G\"");
    assert_eq!(
        one_model("", &big).map(|_| ()),
        Err(ConfigError::PlacementNeverFits {
            model: ModelName::from("laya"),
            placement: p("ram")
        })
    );
}

#[test]
fn a_placement_size_outside_sheps_grammar_names_its_field() {
    let bad = TWO.replace("ram = \"5G\"", "ram = \"5GB\"");
    let Err(ConfigError::Size { field, .. }) = one_model("", &bad) else {
        panic!("5GB is not a size");
    };
    assert_eq!(field, "models.laya.placements.ram.ram");
}

#[test]
fn a_shared_sheep_compares_the_fields_placements_set() {
    let toml = format!(
        r#"
[host]
vram = "24G"
ram = "62G"

[models.tagger]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
ram = "1G"
idle = "8h"

[models.laya]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
{LAYA_PLACEMENTS}"#
    );
    let Err(ConfigError::SharedSheepMismatch { what, .. }) = Config::from_toml(&toml) else {
        panic!("tagger parks no env, script or args on the sheep laya's placements set");
    };
    assert_eq!(what, "env keys");
}

#[test]
fn a_shared_sheep_compares_a_script_or_args_set_only_by_placements() {
    for (field, set) in [
        ("script", "script = \"/opt/serve\""),
        ("args", "args = [\"--cpu\"]"),
    ] {
        let toml = format!(
            r#"
[host]
vram = "24G"
ram = "62G"

[models.tagger]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
ram = "1G"
idle = "8h"

[models.laya]
backend = {{ sheep = "laya" }}
url = "http://127.0.0.1:8000"
idle = "8h"

[[models.laya.placements]]
name = "ram"
ram = "5G"
{set}
"#
        );
        let Err(ConfigError::SharedSheepMismatch { what, .. }) = Config::from_toml(&toml) else {
            panic!("tagger parks no {field} on the sheep laya's placement sets one on");
        };
        assert_eq!(what, field);
    }
}

/// A derived `Debug` would print env values, which can carry credentials.
#[test]
fn a_placements_debug_prints_env_keys_and_an_argument_count_only() {
    let placement = Placement {
        name: p("gpu"),
        footprint: Footprint {
            vram: Vram::None,
            ram: 0,
        },
        script: Some("/opt/serve".to_owned()),
        args: Some(vec!["--api-key".to_owned(), "s3cret".to_owned()]),
        env: BTreeMap::from([("TOKEN".to_owned(), "s3cret".to_owned())]),
    };
    assert_eq!(
        format!("{placement:?}"),
        "Placement { name: PlacementName(\"gpu\"), footprint: Footprint { vram: None, ram: 0 }, \
         script: Some(\"/opt/serve\"), arg_count: Some(2), env_keys: [\"TOKEN\"] }"
    );
}
