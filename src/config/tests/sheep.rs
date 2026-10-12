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
