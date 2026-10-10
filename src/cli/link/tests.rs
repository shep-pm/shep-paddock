use std::{path::Path, time::Duration};

use shep_client::shep_core::secrets::{self, ALL_ENVIRONMENTS};

use super::Link;

fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

/// An environment whose `$SHEP_HOME` is `home`, plus `pairs`
pub(crate) fn env_in(
    home: &Path,
    pairs: &'static [(&'static str, &'static str)],
) -> impl Fn(&str) -> Option<String> {
    let home = home.to_string_lossy().into_owned();
    move |name| match name {
        "SHEP_HOME" => Some(home.clone()),
        _ => pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned()),
    }
}

fn store(home: &Path, key: &str, environment: &str, value: &str) {
    secrets::set(&home.join("secrets.json"), key, environment, value).expect("write the store");
}

fn key_of(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    Link::from_env(env)
        .expect("the store reads")
        .map(|link| link.key)
}

#[test]
fn the_address_defaults_to_the_local_dog() {
    let link = Link::from_env(&env_of(&[("PADDOCK_KEY", "k")]))
        .expect("no store is read")
        .expect("a key is set");
    assert_eq!(link.url, "http://127.0.0.1:8700");
    assert_eq!(link.key, "k");
    assert_eq!(link.retry, Duration::from_secs(2));
    assert_eq!(link.silence, Duration::from_secs(45));
}

#[test]
fn the_address_is_taken_from_the_environment_without_a_trailing_slash() {
    let link = Link::from_env(&env_of(&[
        ("PADDOCK_KEY", "k"),
        ("PADDOCK_URL", "http://gpu-host:8700/"),
    ]))
    .expect("no store is read")
    .expect("a key is set");
    assert_eq!(link.url, "http://gpu-host:8700");
}

#[test]
fn an_unset_or_empty_key_is_no_link() {
    assert!(matches!(Link::from_env(&env_of(&[])), Ok(None)));
    assert!(matches!(
        Link::from_env(&env_of(&[("PADDOCK_KEY", "")])),
        Ok(None)
    ));
}

// Debug is written by hand so the key never reaches a log; a derive would print it.
#[test]
fn a_links_debug_does_not_leak_the_key() {
    let link = Link {
        url: "http://127.0.0.1:8700".to_owned(),
        key: "s3cret-key".to_owned(),
        retry: Duration::from_secs(2),
        silence: Duration::from_secs(45),
    };
    assert_eq!(
        format!("{link:?}"),
        r#"Link { url: "http://127.0.0.1:8700", .. }"#
    );
}

#[test]
fn an_unset_key_comes_from_the_store() {
    let home = tempfile::tempdir().expect("tempdir");
    store(home.path(), "PADDOCK_KEY", ALL_ENVIRONMENTS, "k-stored");
    assert_eq!(
        key_of(&env_in(home.path(), &[])).as_deref(),
        Some("k-stored")
    );
    assert_eq!(
        key_of(&env_in(home.path(), &[("PADDOCK_KEY", "")])).as_deref(),
        Some("k-stored")
    );
}

#[test]
fn the_environments_key_wins_over_the_stores() {
    let home = tempfile::tempdir().expect("tempdir");
    store(home.path(), "PADDOCK_KEY", ALL_ENVIRONMENTS, "k-stored");
    assert_eq!(
        key_of(&env_in(home.path(), &[("PADDOCK_KEY", "k-env")])).as_deref(),
        Some("k-env")
    );
}

#[test]
fn the_store_is_found_under_home_without_shep_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let shep = home.path().join(".shep");
    std::fs::create_dir(&shep).expect("mkdir");
    store(&shep, "PADDOCK_KEY", ALL_ENVIRONMENTS, "k-stored");
    let home_text = home.path().to_string_lossy().into_owned();
    let env = move |name: &str| (name == "HOME").then(|| home_text.clone());
    assert_eq!(key_of(&env).as_deref(), Some("k-stored"));
}

// Only the slot for every environment counts: the CLI has no environment to pick another.
#[test]
fn a_key_stored_for_one_environment_only_is_no_key() {
    let home = tempfile::tempdir().expect("tempdir");
    store(home.path(), "PADDOCK_KEY", "staging", "k-staging");
    assert_eq!(key_of(&env_in(home.path(), &[])), None);
}

#[test]
fn an_absent_store_is_no_key() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(key_of(&env_in(home.path(), &[])), None);
}
