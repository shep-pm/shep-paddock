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

/// `shep adopt` grants a dog only what its manifest declares, and the shepherd refuses every
/// other request the dog sends. Each name here is a request or subscription the dog makes.
#[test]
fn the_manifest_declares_every_permission_the_dog_uses() {
    let output = Command::new(env!("CARGO_BIN_EXE_shep-paddock"))
        .arg("--shep-manifest")
        .output()
        .expect("the binary ran");
    assert!(output.status.success());
    let manifest: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the manifest is JSON");
    let mut declared: Vec<&str> = manifest["permissions"]
        .as_array()
        .expect("the manifest lists permissions")
        .iter()
        .map(|permission| {
            assert_eq!(permission["required"], true, "{permission}");
            permission["name"].as_str().expect("a permission name")
        })
        .collect();
    declared.sort_unstable();
    assert_eq!(
        declared,
        [
            "events:config.dog.",
            "events:process.",
            "flock.control",
            "flock.edit",
            "flock.read",
        ]
    );
}
