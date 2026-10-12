use std::time::Duration;

use super::*;
use crate::footprint::Vram;

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
