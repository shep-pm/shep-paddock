use super::*;

#[test]
fn models_sharing_a_sheep_must_set_the_same_env_keys() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
vram = "all"
idle = "2h"

[models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144", EXTRA = "1" } }
url = "http://127.0.0.1:8080"
vram = "all"
idle = "2h"
"#;
    assert!(matches!(
        Config::from_toml(text),
        Err(ConfigError::SharedSheepMismatch { .. })
    ));
}

#[test]
fn models_sharing_a_sheep_with_the_same_env_keys_are_accepted() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
vram = "all"
idle = "2h"

[models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144" } }
url = "http://127.0.0.1:8080"
vram = "all"
idle = "2h"
"#;
    assert!(Config::from_toml(text).is_ok());
}

#[test]
fn models_sharing_a_sheep_must_all_or_none_set_args() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[models.a]
backend = { sheep = "shared", args = ["--ctx", "1"] }
url = "http://127.0.0.1:8080"
idle = "2h"

[models.b]
backend = { sheep = "shared" }
url = "http://127.0.0.1:8080"
idle = "2h"
"#;
    assert!(matches!(
        Config::from_toml(text),
        Err(ConfigError::SharedSheepMismatch { .. })
    ));
}

/// One model on a sheep, with `backend_env` on the backend and `placement_env` on its one placement.
fn with_env(backend_env: &str, placement_env: &str) -> String {
    format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = {{ sheep = "laya", env = {{ {backend_env} }} }}
url = "http://127.0.0.1:8000"
idle = "8h"

[[models.laya.placements]]
name = "ram"
ram = "5G"
env = {{ {placement_env} }}
"#
    )
}

#[test]
fn an_env_key_starting_with_paddock_is_refused() {
    let text = with_env(r#"PADDOCK_TURN_RAM = "1""#, "");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::ReservedEnvKey { model, key })
            if model == name("laya") && key == "PADDOCK_TURN_RAM"
    ));
}

#[test]
fn a_placement_env_key_starting_with_paddock_is_refused() {
    let text = with_env("", r#"PADDOCK_TURN_RAM = "1""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::ReservedEnvKey { model, key })
            if model == name("laya") && key == "PADDOCK_TURN_RAM"
    ));
}

#[test]
fn a_key_that_only_contains_paddock_is_accepted() {
    let text = with_env(
        r#"MY_PADDOCK_X = "1", paddock_x = "2""#,
        r#"XPADDOCK_ = "3""#,
    );
    // The placement sets a key the backend does not, which one placement may do.
    assert!(Config::from_toml(&text).is_ok());
}

#[test]
fn the_reserved_key_error_never_prints_a_value() {
    for text in [
        with_env(r#"PADDOCK_TOKEN = "hunter2""#, ""),
        with_env("", r#"PADDOCK_TOKEN = "hunter2""#),
    ] {
        let err = Config::from_toml(&text).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("PADDOCK_TOKEN"), "{shown}");
    }
}

/// Two models on the sheep `shared`, each with `extra` after its own keys.
fn two_on_a_sheep(a_extra: &str, b_extra: &str) -> String {
    format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.a]
backend = {{ sheep = "shared" }}
url = "http://127.0.0.1:8080"
idle = "2h"
{a_extra}
[models.b]
backend = {{ sheep = "shared" }}
url = "http://127.0.0.1:8080"
idle = "2h"
{b_extra}
"#
    )
}

#[test]
fn models_sharing_a_sheep_must_all_or_none_declare_turn_ram() {
    let text = two_on_a_sheep("sequences = 1\nturn_ram = \"1G\"", "sequences = 1");
    let Err(ConfigError::SharedSheepMismatch {
        sheep,
        first,
        second,
        what,
    }) = Config::from_toml(&text)
    else {
        panic!("b parks no turn_ram on the sheep a parks one on");
    };
    assert_eq!(sheep, "shared");
    assert_eq!(first, name("a"));
    assert_eq!(second, name("b"));
    assert_eq!(what, "turn_ram");
}

#[test]
fn models_sharing_a_sheep_that_all_declare_turn_ram_are_accepted() {
    let text = two_on_a_sheep(
        "sequences = 1\nturn_ram = \"1G\"",
        "sequences = 2\nturn_ram = \"2G\"",
    );
    assert!(Config::from_toml(&text).is_ok());
}

#[test]
fn a_sheep_model_without_url_is_refused() {
    let text = MINIMAL.replace("url = \"http://127.0.0.1:8000\"\n", "");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::MissingUrl { .. })
    ));
}

#[test]
fn a_url_that_is_not_one_with_a_host_is_a_config_error() {
    for bad in [
        "not a url",
        "unix:/run/laya.sock",
        "http://",
        "ftp://127.0.0.1:8000",
        "ws://127.0.0.1:8000",
    ] {
        let text = MINIMAL.replace(
            r#"url = "http://127.0.0.1:8000""#,
            &format!("url = \"{bad}\""),
        );
        assert!(
            matches!(
                Config::from_toml(&text),
                Err(ConfigError::BadUrl { model }) if model == name("laya")
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn an_https_url_is_accepted() {
    let text = MINIMAL.replace("http://127.0.0.1:8000", "https://127.0.0.1:8000");
    assert!(Config::from_toml(&text).is_ok());
}

#[test]
fn the_forwarding_base_is_parsed_once_and_trimmed() {
    let text = MINIMAL.replace("http://127.0.0.1:8000", "http://127.0.0.1:8000/api//");
    let config = Config::from_toml(&text).unwrap();
    let base = config.models[&name("laya")].base.as_ref().unwrap();
    assert_eq!(base.as_str(), "http://127.0.0.1:8000/api");
}
