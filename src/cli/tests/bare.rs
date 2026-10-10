//! Parsing `run` for a bare lease.

use super::*;
use crate::cli::STOP_GRACE;

#[test]
fn run_takes_a_bare_lease_with_vram_ram_and_grace() {
    let parsed = run_args(&[
        "run",
        "--vram",
        "12G",
        "--ram",
        "4G",
        "--grace",
        "1m",
        "--",
        "./train.sh",
    ]);
    assert_eq!(parsed.model, None);
    assert_eq!(parsed.vram.as_deref(), Some("12G"));
    assert_eq!(parsed.ram.as_deref(), Some("4G"));
    assert_eq!(parsed.grace, Duration::from_secs(60));
}

#[test]
fn a_bare_run_takes_either_figure_alone_and_waits_thirty_seconds_by_default() {
    let all = run_args(&["run", "--vram", "all", "--", "true"]);
    assert_eq!((all.vram.as_deref(), all.ram), (Some("all"), None));
    assert_eq!(all.grace, STOP_GRACE);
    assert_eq!(STOP_GRACE, Duration::from_secs(30));
    let ram = run_args(&["run", "--ram", "512M", "--", "true"]);
    assert_eq!((ram.vram, ram.ram.as_deref()), (None, Some("512M")));
}

#[test]
fn a_model_and_a_footprint_are_exclusive_and_one_is_required() {
    assert!(refused(&["run", "--model", "m", "--vram", "8G", "--", "true"]).contains("exclusive"));
    assert!(
        refused(&["run", "--", "true"])
            .starts_with("--model is required, or --vram, --ram or both for a bare lease.\n")
    );
}

#[test]
fn a_size_or_grace_outside_shep_grammar_is_refused() {
    assert!(refused(&["run", "--vram", "8 GB", "--", "true"]).contains("--vram is not a size"));
    assert!(refused(&["run", "--ram", "all", "--", "true"]).contains("--ram is not a size"));
    assert!(
        refused(&["run", "--vram", "8G", "--grace", "soon", "--", "true"])
            .contains("--grace is not a duration")
    );
}

#[test]
fn each_kind_of_lease_refuses_the_other_kinds_flags() {
    for words in [
        &["run", "--vram", "8G", "--reclaimable", "--", "true"][..],
        &[
            "run",
            "--vram",
            "8G",
            "--release-if-idle",
            "30m",
            "--",
            "true",
        ][..],
        &["run", "--model", "m", "--grace", "10s", "--", "true"][..],
    ] {
        assert!(refused(words).contains("is for a"), "{words:?}");
    }
}
