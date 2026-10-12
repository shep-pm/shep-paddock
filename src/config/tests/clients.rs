use super::*;

#[test]
fn a_client_with_an_empty_key_is_refused() {
    let text = MINIMAL.replace(r#"key = "k-bench""#, r#"key = """#);
    assert!(matches!(
        Config::from_toml(&text),
        Err(ConfigError::EmptyKey { .. })
    ));
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

#[test]
fn clients_that_differ_in_admin_or_protected_differ_but_their_keys_are_not_compared() {
    let plain = Client::with_key(ClientName::from("mac-sessions"), "k-mac");
    let admin = Client {
        admin: true,
        ..plain.clone()
    };
    let protected = Client {
        protected: true,
        ..plain.clone()
    };
    assert_ne!(plain, admin);
    assert_ne!(plain, protected);
    assert_eq!(
        plain,
        Client::with_key(ClientName::from("mac-sessions"), "k-other")
    );
}
