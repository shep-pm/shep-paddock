//! The key from shep's secret store, when `$PADDOCK_KEY` is unset.

use std::path::Path;

use shep_client::shep_core::secrets::{self, ALL_ENVIRONMENTS};

use super::{Command, Link, execute, quiet};

/// An environment whose `$SHEP_HOME` is `home`, plus `pairs`
fn env_in(
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

#[tokio::test]
async fn an_unreadable_store_says_so_and_exits_1() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(home.path().join("secrets.json"), "not json").expect("write");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(
        &env_in(home.path(), &[]),
        Command::Status,
        &mut out,
        &mut err,
        &mut quiet(),
    )
    .await;
    assert_eq!(code, 1);
    let said = String::from_utf8_lossy(&err);
    assert!(
        said.starts_with("paddock: cannot read shep's secret store: "),
        "{said}"
    );
    assert!(out.is_empty());
}

#[tokio::test]
async fn no_key_anywhere_names_both_places() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(
        &env_in(home.path(), &[]),
        Command::Status,
        &mut out,
        &mut err,
        &mut quiet(),
    )
    .await;
    assert_eq!(code, 2);
    assert_eq!(
        String::from_utf8_lossy(&err),
        "paddock: $PADDOCK_KEY is not set, and shep's secret store holds no PADDOCK_KEY. \
         Either is this client's key.\n"
    );
}
