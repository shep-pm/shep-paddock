use std::time::Duration;

use super::*;
use crate::footprint::Vram;

const MINIMAL: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "bench-01"
key = "k-bench"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
ram = "5G"
idle = "8h"
"#;

fn name(text: &str) -> ModelName {
    ModelName::from(text)
}

/// MINIMAL plus a second sheep model, `iq3_s`, that holds all the VRAM.
fn with_iq3_s(extra: &str) -> String {
    format!(
        "{MINIMAL}
[models.iq3_s]
backend = {{ sheep = \"iq3_s\" }}
url = \"http://127.0.0.1:8080\"
vram = \"all\"
ram = \"20G\"
idle = \"2h\"
{extra}
"
    )
}

#[test]
fn defaults_fill_in_what_the_section_leaves_out() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert_eq!(config.listen, "0.0.0.0:8700".parse().unwrap());
    assert_eq!(config.grace, Duration::from_secs(120));
    assert_eq!(config.max_wait, Duration::from_secs(120));
    assert_eq!(config.reconnect, Duration::from_secs(60));
    let laya = &config.models[&name("laya")];
    assert_eq!(laya.load_timeout, Duration::from_secs(300));
    assert_eq!(laya.footprint.vram, Vram::None);
    assert_eq!(laya.idle, Duration::from_secs(8 * 3600));
}

#[test]
fn a_size_outside_shep_grammar_is_refused() {
    let text = MINIMAL.replace(r#"ram = "5G""#, r#"ram = "5 GB""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::Size { .. })
    ));
}

#[test]
fn a_duration_outside_shep_grammar_is_refused() {
    let text = MINIMAL.replace(r#"idle = "8h""#, r#"idle = "8 hours""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::Duration { .. })
    ));
}

#[test]
fn vram_all_is_the_only_word_a_size_accepts() {
    let text = MINIMAL.replace(r#"ram = "5G""#, "ram = \"5G\"\nvram = \"all\"");
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(config.models[&name("laya")].footprint.vram, Vram::All);

    let text = MINIMAL.replace(r#"ram = "5G""#, "ram = \"5G\"\nvram = \"most\"");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::Size { .. })
    ));
}

#[test]
fn a_listen_address_that_does_not_parse_is_refused() {
    let text = format!("listen = \"everywhere\"\n{MINIMAL}");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::Listen { .. })
    ));
}

#[test]
fn exclusions_apply_both_ways() {
    let text = with_iq3_s(r#"excludes = ["laya"]"#);
    let config = Config::from_toml(&text).unwrap();
    assert!(config.excluded(&name("laya"), &name("iq3_s")));
    assert!(config.excluded(&name("iq3_s"), &name("laya")));
    assert!(!config.excluded(&name("laya"), &name("laya")));
}

#[test]
fn an_exclusion_naming_an_unknown_model_is_refused() {
    let text = with_iq3_s(r#"excludes = ["nobody"]"#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::UnknownExclusion { .. })
    ));
}

#[test]
fn a_model_on_an_unknown_backend_is_refused() {
    let text = MINIMAL.replace(r#"backend = { sheep = "laya" }"#, r#"backend = "nope""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::UnknownBackend { .. })
    ));
}

#[test]
fn two_models_with_one_prefix_are_refused() {
    let text = with_iq3_s("").replace("vram = \"all\"", "vram = \"all\"\nprefix = \"/laya\"");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::DuplicatePrefix { .. })
    ));
}

#[test]
fn a_prefix_off_a_segment_boundary_is_refused() {
    for prefix in ["", "/", "laya", "/laya/"] {
        let text = MINIMAL.replace(r#"prefix = "/laya""#, &format!("prefix = \"{prefix}\""));
        assert!(
            matches!(
                Config::from_toml(&text),
                Err(ConfigError::BadPrefix { prefix: refused, .. }) if refused == prefix
            ),
            "{prefix:?}"
        );
    }
}

#[test]
fn a_client_with_an_empty_key_is_refused() {
    let text = MINIMAL.replace(r#"key = "k-bench""#, r#"key = """#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::EmptyKey { .. })
    ));
}

#[test]
fn a_model_that_never_fits_the_host_is_refused() {
    let text = MINIMAL.replace(r#"ram = "5G""#, r#"ram = "100G""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::NeverFits { .. })
    ));
}

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
apis = ["openai"]
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
    assert_eq!(model.apis, vec![Api::OpenAi]);
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
fn unknown_fields_are_refused() {
    let text = MINIMAL.replace(r#"ram = "5G""#, r#"rams = "5G""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::Toml(_))
    ));
}

#[test]
fn debug_does_not_print_client_or_model_keys() {
    let text = MINIMAL.replace(
        r#"prefix = "/laya""#,
        "prefix = \"/laya\"\nkey = \"k-laya-secret\"",
    );
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(
        format!("{:?}", config.clients[0]),
        r#"Client { name: ClientName("bench-01"), .. }"#
    );
    assert_eq!(
        format!("{:?}", config.models[&name("laya")]),
        concat!(
            r#"Model { name: ModelName("laya"), "#,
            r#"backend: Sheep { sheep: "laya", args: None, env_keys: [] }, "#,
            r#"url: Some("http://127.0.0.1:8000"), ready: None, apis: [], "#,
            r#"prefix: Some("/laya"), "#,
            "footprint: Footprint { vram: None, ram: 5368709120 }, ",
            "excludes: {}, idle: 28800s, load_timeout: 300s, .. }"
        )
    );
}

#[test]
fn debug_does_not_print_backend_environment_values() {
    let text = MINIMAL.replace(
        r#"{ sheep = "laya" }"#,
        r#"{ sheep = "laya", env = { TOKEN = "hunter2" } }"#,
    );
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(
        format!("{:?}", config.models[&name("laya")].backend),
        r#"Sheep { sheep: "laya", args: None, env_keys: ["TOKEN"] }"#
    );
}

#[test]
fn a_key_matches_only_itself() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert!(config.client_for_key(b"k-bench").is_some());
    assert!(config.client_for_key(b"k-benc").is_none());
    assert!(config.client_for_key(b"k-benchh").is_none());
    assert!(config.client_for_key(b"").is_none());
}

#[test]
fn a_models_key_is_readable_by_the_dog() {
    let text = MINIMAL.replace(
        r#"prefix = "/laya""#,
        "prefix = \"/laya\"\nkey = \"k-laya\"",
    );
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(config.models[&name("laya")].key(), Some("k-laya"));
}

#[test]
fn a_prefix_matches_only_on_a_segment_boundary() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert!(config.model_for_prefix("/laya/v1/systemone").is_some());
    assert!(config.model_for_prefix("/laya").is_some());
    assert!(config.model_for_prefix("/layabout").is_none());
}

#[test]
fn the_schema_marks_both_keys_as_secret() {
    let schema = shep_client::dogs::config_schema::<section::Section>();
    let schema = schema.as_value();
    let secret = Some(&serde_json::Value::Bool(true));
    let marked = |def: &str| {
        schema.pointer(&format!(
            "/$defs/{def}/properties/key/{}",
            shep_client::dogs::SECRET_KEY
        ))
    };
    assert_eq!(marked("ClientSection"), secret);
    assert_eq!(marked("ModelSection"), secret);
}

#[test]
fn the_schema_accepts_all_for_vram_and_not_for_ram() {
    let schema = shep_client::dogs::config_schema::<section::Section>();
    let rendered = schema.as_value().to_string();
    assert!(rendered.contains(r"^(\\d+(G|M|K)?|all)$"), "{rendered}");
    let host_ram = schema
        .as_value()
        .pointer("/$defs/HostSection/properties/ram");
    assert!(
        !host_ram.unwrap().to_string().contains("all"),
        "{host_ram:?}"
    );
}

#[test]
fn the_shared_fixture_is_a_valid_config() {
    let config = crate::test_support::config(crate::test_support::HOST_AND_MODELS);
    assert_eq!(config.models.len(), 5);
    assert!(config.client_for_key(b"k-mac").is_some());
    assert!(config.client_for_key(b"k-bench").is_some());
    assert_eq!(config.clients[0].name.as_str(), "mac-sessions");
    assert_eq!(name("laya").as_str(), "laya");
    assert_eq!(name("laya").to_string(), "laya");
}

#[test]
fn a_misspelled_key_line_is_not_echoed() {
    let text = MINIMAL.replace(
        "prefix = \"/laya\"",
        "prefix = \"/laya\"\nkye = \"k-secret-xyz\"",
    );
    let err = Config::from_toml(&text).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("k-secret-xyz"), "{shown}");
    assert!(shown.contains("kye"), "{shown}");
    assert!(shown.contains("line"), "{shown}");
}

#[test]
fn a_non_string_key_value_is_not_echoed() {
    let text = MINIMAL.replace(r#"key = "k-bench""#, "key = 12345");
    let err = Config::from_toml(&text).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("12345"), "{shown}");
    assert!(matches!(err, ConfigError::Toml(_)));
}
