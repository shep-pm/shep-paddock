//! A model's container.

use super::super::{Config, ConfigError, ModelName};

/// One Strata model, as the maintainer names its container on the GPU host.
const STRATA: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.iq3_xxs]
backend = { sheep = "iq3_xxs" }
url = "http://127.0.0.1:8080"
vram = "all"
ram = "55G"
idle = "2h"
container = "strata-qwen-iq3_xxs"
"#;

fn refused(text: &str) -> ConfigError {
    match Config::from_toml(text) {
        Ok(_) => panic!("accepted:\n{text}"),
        Err(err) => err,
    }
}

#[test]
fn a_sheep_model_may_name_its_container() {
    let config = Config::from_toml(STRATA).expect("valid");
    let model = &config.models[&ModelName::from("iq3_xxs")];
    assert_eq!(model.container.as_deref(), Some("strata-qwen-iq3_xxs"));
}

#[test]
fn a_container_on_an_ollama_model_is_refused() {
    let text = format!(
        "{STRATA}\n[models.qwen]\nbackend = \"ollama\"\nname = \"qwen3.8:27b\"\nvram = \"20G\"\nidle = \"2h\"\ncontainer = \"qwen\"\n"
    );
    assert_eq!(
        refused(&text),
        ConfigError::ContainerOnOllama {
            model: ModelName::from("qwen")
        }
    );
}

#[test]
fn a_container_name_podman_would_not_give_is_refused() {
    for bad in [
        "",
        "-rm",
        ".hidden",
        "has space",
        "ok/slash",
        "émile",
        "strata-é",
    ] {
        let text = STRATA.replace("strata-qwen-iq3_xxs", bad);
        assert_eq!(
            refused(&text),
            ConfigError::BadContainer {
                model: ModelName::from("iq3_xxs"),
                container: bad.to_owned()
            },
            "{bad:?}"
        );
    }
    let said = refused(&STRATA.replace("strata-qwen-iq3_xxs", "émile")).to_string();
    assert!(
        said.ends_with("an ASCII letter or digit, then ASCII letters, digits, _, . or -"),
        "{said}"
    );
}

#[test]
fn models_on_one_sheep_must_name_the_same_container() {
    let other = "\n[models.iq3_xxs-256k]\nbackend = { sheep = \"iq3_xxs\" }\nurl = \"http://127.0.0.1:8080\"\nvram = \"all\"\nram = \"60G\"\nidle = \"2h\"\n";
    let text = format!("{STRATA}{other}");
    assert_eq!(
        refused(&text),
        ConfigError::SharedSheepMismatch {
            sheep: "iq3_xxs".to_owned(),
            first: ModelName::from("iq3_xxs"),
            second: ModelName::from("iq3_xxs-256k"),
            what: "container",
        }
    );
    let same = format!("{text}container = \"strata-qwen-iq3_xxs\"\n");
    assert!(Config::from_toml(&same).is_ok());
}

#[test]
fn one_container_on_two_sheep_is_refused() {
    let other = "\n[models.iq2_xs]\nbackend = { sheep = \"iq2_xs\" }\nurl = \"http://127.0.0.1:8081\"\nvram = \"all\"\nram = \"37G\"\nidle = \"2h\"\ncontainer = \"strata-qwen-iq3_xxs\"\n";
    assert_eq!(
        refused(&format!("{STRATA}{other}")),
        ConfigError::SharedContainer {
            container: "strata-qwen-iq3_xxs".to_owned(),
            first: ModelName::from("iq2_xs"),
            second: ModelName::from("iq3_xxs"),
        }
    );
}
