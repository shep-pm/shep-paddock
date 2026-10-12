//! What the built binary answers `shep adopt`'s probes with.

use std::process::Command;

/// `shep adopt` reads this line to decide whether to start the dog with the shepherd channel.
#[test]
fn the_version_answer_asks_for_the_channel() {
    let output = Command::new(env!("CARGO_BIN_EXE_shep-paddock"))
        .arg("--version")
        .output()
        .expect("the binary ran");
    assert!(output.status.success());
    let answer = String::from_utf8_lossy(&output.stdout);
    assert!(
        answer.lines().any(|line| line == "shep-channel: true"),
        "{answer}"
    );
}
