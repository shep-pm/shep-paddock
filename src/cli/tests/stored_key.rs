//! What the command line says when no key can be found.

use super::{Command, execute, quiet};
use crate::cli::link::tests::env_in;

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
         Set either to this client's key.\n"
    );
}
