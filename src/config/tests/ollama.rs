use super::*;
use crate::footprint::Vram;

#[test]
fn an_ollama_model_takes_the_backend_url() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
apis = ["openai", "ollama"]
vram = "22323M"
ram = "4G"
idle = "2h"
"#;
    let config = Config::from_toml(text).unwrap();
    let model = &config.models[&name("qwen3.8:27b")];
    assert_eq!(
        model.backend,
        Backend::Ollama {
            url: "http://127.0.0.1:11434".to_owned(),
            name: "qwen3.8:27b-ctx131072".to_owned(),
        }
    );
    assert_eq!(model.apis, vec![Api::OpenAi, Api::Ollama]);
    assert_eq!(model.footprint.vram, Vram::Bytes(22323 * 1024 * 1024));
}

#[test]
fn an_ollama_model_without_a_name_is_refused() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.q]
backend = "ollama"
idle = "2h"
"#;
    assert!(matches!(
        Config::from_toml(text),
        Err(ConfigError::MissingName { .. })
    ));
}

#[test]
fn a_trailing_slash_on_a_url_is_dropped() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434/"

[models.qwen]
backend = "ollama"
name = "qwen"
ram = "4G"
idle = "2h"

[models.other]
backend = "ollama"
name = "other"
url = "http://127.0.0.1:11435//"
ram = "4G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000/"
ram = "5G"
idle = "8h"
"#;
    let config = Config::from_toml(text).unwrap();
    let model = |model: &str| &config.models[&name(model)];
    assert_eq!(
        model("qwen").backend,
        Backend::Ollama {
            url: "http://127.0.0.1:11434".to_owned(),
            name: "qwen".to_owned(),
        }
    );
    assert_eq!(model("qwen").url.as_deref(), Some("http://127.0.0.1:11434"));
    assert_eq!(
        model("other").url.as_deref(),
        Some("http://127.0.0.1:11435")
    );
    assert_eq!(model("laya").url.as_deref(), Some("http://127.0.0.1:8000"));
}

#[test]
fn two_models_naming_one_ollama_model_are_refused() {
    let refused = Config::from_toml(&two_ollama_models("qwen3:latest", "ollama"));

    let Err(err) = refused else {
        panic!("accepted: {refused:?}");
    };
    assert!(
        matches!(
            &err,
            ConfigError::SharedOllamaModel { first, second, .. }
                if *first == name("q") && *second == name("r")
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "models \"q\" and \"r\" both name ollama model \"qwen3:latest\" at http://127.0.0.1:11434"
    );
}

#[test]
fn the_shared_model_error_leaves_out_the_urls_password() {
    let text = two_ollama_models("qwen3:latest", "ollama").replace(
        "http://127.0.0.1:11434",
        "http://user:s3cret@127.0.0.1:11434",
    );

    let err = Config::from_toml(&text).expect_err("two models on one ollama model");

    assert_eq!(
        err.to_string(),
        "models \"q\" and \"r\" both name ollama model \"qwen3:latest\" at http://127.0.0.1:11434"
    );
}

#[test]
fn one_ollama_model_on_two_servers_is_accepted() {
    assert!(Config::from_toml(&two_ollama_models("qwen3", "other")).is_ok());
}

#[test]
fn a_bad_ollama_backend_url_names_the_model_and_never_prints_the_url() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "://user:s3cret@"

[models.q]
backend = "ollama"
name = "q"
idle = "2h"
"#;
    let Err(err) = Config::from_toml(text) else {
        panic!("a url that does not parse is refused");
    };
    assert_eq!(err, ConfigError::BadUrl { model: name("q") });
    assert!(!err.to_string().contains("s3cret"), "{err}");

    let text = text.replace("://user:s3cret@", "ftp://127.0.0.1:11434");
    assert_eq!(
        Config::from_toml(&text),
        Err(ConfigError::BadUrl { model: name("q") })
    );
}
