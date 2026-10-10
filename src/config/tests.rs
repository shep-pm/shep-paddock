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
fn models_on_one_sheep_exclude_each_other() {
    let laya_b = r#"
[models.laya-b]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;
    let config = Config::from_toml(&with_iq3_s(laya_b)).unwrap();
    assert!(config.excluded(&name("laya"), &name("laya-b")));
    assert!(config.excluded(&name("laya-b"), &name("laya")));
    assert!(!config.excluded(&name("laya"), &name("iq3_s")));
    assert!(!config.excluded(&name("laya-b"), &name("laya-b")));
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
            r#"backend: Sheep { sheep: "laya", name: None, script: None, arg_count: None, env_keys: [] }, "#,
            r#"url: Some("http://127.0.0.1:8000"), ready: None, apis: [], "#,
            r#"prefix: Some("/laya"), "#,
            "footprint: Footprint { vram: None, ram: 5368709120 }, ",
            "placements: [], ",
            "excludes: {}, idle: 28800s, load_timeout: 300s, sequences: None, container: None, .. }"
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
        r#"Sheep { sheep: "laya", name: None, script: None, arg_count: None, env_keys: ["TOKEN"] }"#
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
    let value = schema.as_value();
    let pattern = |def: &str| {
        value
            .pointer(&format!("/$defs/{def}/pattern"))
            .and_then(serde_json::Value::as_str)
    };
    assert_eq!(pattern("MemSize"), Some(r"^\d+(G|M|K)?$"));
    let mem = Some("#/$defs/MemSize");
    let reference = |pointer: &str| value.pointer(pointer).and_then(serde_json::Value::as_str);
    assert_eq!(reference("/$defs/HostSection/properties/ram/$ref"), mem);
    assert_eq!(
        reference("/$defs/ModelSection/properties/ram/anyOf/0/$ref"),
        mem
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

#[test]
fn the_readme_example_parses() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("the README");
    let block = readme
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the README has a toml block");
    // The shepherd hands the dog its section without the header, so the header goes and each
    // sub-table loses its `paddock.` prefix.
    let section = block
        .replace("[paddock]\n", "")
        .replace("[[paddock.", "[[")
        .replace("[paddock.", "[");

    let config = Config::from_toml(&section).expect("the README's example is a valid section");

    assert_eq!(config.models.len(), 1);
}

#[test]
fn a_span_inside_a_multibyte_character_is_not_located() {
    let Err(err) = toml::from_str::<Section>("listen = 1\n") else {
        panic!("a number is not a listen address");
    };
    let start = err.span().expect("a span").start;
    // `é` is two bytes, so an odd offset into a run of them falls inside one.
    let pad = if start % 2 == 1 { "" } else { "a" };
    let text = format!("{pad}{}", "é".repeat(start + 2));

    let located = ConfigError::from_toml_error(&err, &text);

    assert!(
        matches!(&located, ConfigError::Toml(shown) if shown == "a value of the wrong type or form"),
        "{located:?}"
    );
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

/// Two ollama models on `backends.ollama`, named `first` and `second` to ollama.
fn two_ollama_models(first: &str, second: &str) -> String {
    format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[backends.other]
kind = "ollama"
url = "http://127.0.0.1:11435"

[models.q]
backend = "ollama"
name = "{first}"
vram = "10G"
idle = "2h"

[models.r]
backend = "{second}"
name = "qwen3"
vram = "10G"
idle = "2h"
"#
    )
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
fn two_clients_with_one_name_are_refused() {
    let text = format!("{MINIMAL}\n[[clients]]\nname = \"bench-01\"\nkey = \"k-other\"\n");
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::DuplicateClientName { name }) if name == "bench-01".into()
    ));
}

#[test]
fn two_clients_with_one_key_are_refused_without_printing_it() {
    let text = format!("{MINIMAL}\n[[clients]]\nname = \"bench-02\"\nkey = \"k-bench\"\n");
    let Err(err) = Config::from_toml(&text) else {
        panic!("one key for two clients is refused");
    };
    assert!(matches!(
        &err,
        ConfigError::DuplicateClientKey { first, second }
            if *first == "bench-01".into() && *second == "bench-02".into()
    ));
    assert!(!err.to_string().contains("k-bench"), "{err}");
}

#[test]
fn a_prefix_inside_another_is_refused_but_a_sibling_sharing_letters_is_not() {
    let nested = with_iq3_s("").replace("vram = \"all\"", "vram = \"all\"\nprefix = \"/laya/x\"");
    assert!(matches!(
        Config::from_toml(&nested),
        Err(ConfigError::OverlappingPrefix { outer, inner, .. })
            if outer == "/laya" && inner == "/laya/x"
    ));

    let sibling = with_iq3_s("").replace("vram = \"all\"", "vram = \"all\"\nprefix = \"/layaa\"");
    assert!(Config::from_toml(&sibling).is_ok());
}

#[test]
fn clients_compare_by_name_alone() {
    let a = Client::with_key(name_of("bench"), "one");
    let same_name = Client::with_key(name_of("bench"), "two");
    let other = Client::with_key(name_of("other"), "one");
    assert_eq!(a, same_name);
    assert_ne!(a, other);
}

#[test]
fn a_client_is_an_admin_only_when_it_says_so() {
    let text = MINIMAL.replace(
        "key = \"k-bench\"",
        "key = \"k-bench\"\n\n[[clients]]\nname = \"mac-sessions\"\nkey = \"k-mac\"\nadmin = true",
    );
    let config = Config::from_toml(&text).unwrap();
    let admin = |name: &str| {
        config
            .clients
            .iter()
            .find(|client| client.name == name_of(name))
            .map(|client| client.admin)
    };
    assert_eq!(admin("bench-01"), Some(false));
    assert_eq!(admin("mac-sessions"), Some(true));
}

#[test]
fn a_client_is_protected_only_when_it_says_so() {
    let text = MINIMAL.replace(
        "key = \"k-bench\"",
        "key = \"k-bench\"\n\n[[clients]]\nname = \"maintainer\"\nkey = \"k-main\"\nadmin = true\nprotected = true",
    );
    let config = Config::from_toml(&text).unwrap();
    let protected = |name: &str| {
        config
            .clients
            .iter()
            .find(|client| client.name == name_of(name))
            .map(|client| client.protected)
    };
    assert_eq!(protected("bench-01"), Some(false));
    assert_eq!(protected("maintainer"), Some(true));
}

fn name_of(text: &str) -> ClientName {
    ClientName::from(text)
}

#[test]
fn an_error_can_be_cloned_and_compared_whole() {
    let text = MINIMAL.replace(r#"idle = "8h""#, r#"idle = "8 hours""#);
    let err = Config::from_toml(&text).unwrap_err();
    assert_eq!(err.clone(), err);
    assert_ne!(err, Config::from_toml("listen = 1").unwrap_err());
}

#[test]
fn the_column_counts_characters_not_bytes() {
    let text = "host = { vram = \"ééé\", ramm = \"1G\" }\n";
    let column = text[..text.find("ramm").unwrap()].chars().count() + 1;
    let Err(ConfigError::Toml(shown)) = Config::from_toml(text) else {
        panic!("a misspelled key is refused");
    };
    assert!(
        shown.ends_with(&format!("at line 1, column {column}")),
        "{shown}"
    );
}

#[test]
fn a_typo_inside_an_inline_sheep_names_the_field() {
    let text = MINIMAL.replace(
        r#"{ sheep = "laya" }"#,
        r#"{ sheep = "laya", argz = ["--x"] }"#,
    );
    let Err(ConfigError::Toml(shown)) = Config::from_toml(&text) else {
        panic!("a misspelled sheep field is refused");
    };
    assert!(shown.starts_with("unknown field `argz`"), "{shown}");

    let text = MINIMAL.replace(r#"{ sheep = "laya" }"#, r#"{ args = ["--x"] }"#);
    let Err(ConfigError::Toml(shown)) = Config::from_toml(&text) else {
        panic!("a sheep with no name is refused");
    };
    assert!(shown.starts_with("missing field `sheep`"), "{shown}");
}

#[test]
fn debug_does_not_print_backend_arguments() {
    let text = MINIMAL.replace(
        r#"{ sheep = "laya" }"#,
        r#"{ sheep = "laya", args = ["--api-key", "hunter2"] }"#,
    );
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(
        format!("{:?}", config.models[&name("laya")].backend),
        r#"Sheep { sheep: "laya", name: None, script: None, arg_count: Some(2), env_keys: [] }"#
    );
}

#[test]
fn debug_does_not_print_a_urls_password() {
    let text = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://paddock:hunter2@127.0.0.1:11434"

[models.qwen]
backend = "ollama"
name = "qwen"
ram = "4G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://paddock:hunter2@127.0.0.1:8000"
ram = "5G"
idle = "8h"
"#;
    let config = Config::from_toml(text).unwrap();
    assert_eq!(
        format!("{:?}", config.models[&name("qwen")].backend),
        r#"Ollama { url: "http://127.0.0.1:11434", name: "qwen" }"#
    );
    assert_eq!(
        format!("{:?}", config.models[&name("laya")]),
        concat!(
            r#"Model { name: ModelName("laya"), "#,
            r#"backend: Sheep { sheep: "laya", name: None, script: None, arg_count: None, env_keys: [] }, "#,
            r#"url: Some("http://127.0.0.1:8000"), ready: None, apis: [], prefix: None, "#,
            "footprint: Footprint { vram: None, ram: 5368709120 }, ",
            "placements: [], ",
            "excludes: {}, idle: 28800s, load_timeout: 300s, sequences: None, container: None, .. }"
        )
    );
    let shown = format!("{config:?}");
    assert!(
        shown.contains(r#"ollamas: {"http://127.0.0.1:11434": "ollama"}"#),
        "{shown}"
    );
    assert!(!shown.contains("hunter2"), "{shown}");
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

#[test]
fn sequences_limit_a_models_leases_and_are_unlimited_when_unset() {
    let config = Config::from_toml(&with_iq3_s("sequences = 2")).unwrap();
    let limit = |model: &str| config.models[&name(model)].sequences.map(|n| n.get());
    assert_eq!(limit("iq3_s"), Some(2));
    assert_eq!(limit("laya"), None);
}

#[test]
fn zero_sequences_are_refused() {
    assert!(Config::from_toml(&with_iq3_s("sequences = 0")).is_err());
}
