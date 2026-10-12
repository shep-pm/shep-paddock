use super::*;

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
            "excludes: {}, idle: 28800s, load_timeout: 300s, sequences: None, container: None, turn_ram: None, .. }"
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
            "excludes: {}, idle: 28800s, load_timeout: 300s, sequences: None, container: None, turn_ram: None, .. }"
        )
    );
    let shown = format!("{config:?}");
    assert!(
        shown.contains(r#"ollamas: {"http://127.0.0.1:11434": "ollama"}"#),
        "{shown}"
    );
    assert!(!shown.contains("hunter2"), "{shown}");
}
