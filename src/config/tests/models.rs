use super::*;

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
fn a_model_that_never_fits_the_host_is_refused() {
    let text = MINIMAL.replace(r#"ram = "5G""#, r#"ram = "100G""#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::NeverFits { .. })
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
fn a_prefix_matches_only_on_a_segment_boundary() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert!(config.model_for_prefix("/laya/v1/systemone").is_some());
    assert!(config.model_for_prefix("/laya").is_some());
    assert!(config.model_for_prefix("/layabout").is_none());
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
