use std::time::Duration;

use super::{Command, Forward, Link, RunArgs, execute, forwarded_signals, parse};

/// A signal source that never fires.
fn quiet() -> tokio::sync::mpsc::UnboundedReceiver<Forward> {
    tokio::sync::mpsc::unbounded_channel().1
}

fn args(words: &[&str]) -> Result<Command, super::Usage> {
    parse(words.iter().copied())
}

fn run_args(words: &[&str]) -> RunArgs {
    match args(words) {
        Ok(Command::Run(run)) => run,
        other => panic!("{words:?} did not parse as run: {other:?}"),
    }
}

fn refused(words: &[&str]) -> String {
    match args(words) {
        Err(usage) => usage.to_string(),
        Ok(parsed) => panic!("{words:?} should be refused, parsed as {parsed:?}"),
    }
}

#[test]
fn status_takes_no_arguments() {
    assert_eq!(args(&["status"]), Ok(Command::Status));
    assert!(refused(&["status", "now"]).contains("status takes no arguments"));
}

#[test]
fn run_reads_every_flag_and_the_command_after_the_dashes() {
    let parsed = run_args(&[
        "run",
        "--model",
        "iq2_xs",
        "--expected",
        "8h",
        "--note",
        "strata run 3",
        "--interactive",
        "--",
        "make",
        "bench",
    ]);
    assert_eq!(
        parsed,
        RunArgs {
            model: "iq2_xs".to_owned(),
            expected: Some("8h".to_owned()),
            note: Some("strata run 3".to_owned()),
            interactive: true,
            command: vec!["make".to_owned(), "bench".to_owned()],
        }
    );
}

#[test]
fn run_defaults_to_a_batch_lease_with_no_estimate_or_note() {
    let parsed = run_args(&["run", "--model", "m", "--", "true"]);
    assert_eq!(parsed.expected, None);
    assert_eq!(parsed.note, None);
    assert!(!parsed.interactive);
}

#[test]
fn flags_may_come_in_any_order() {
    let parsed = run_args(&[
        "run",
        "--interactive",
        "--note",
        "n",
        "--model",
        "m",
        "--",
        "c",
    ]);
    assert_eq!((parsed.model.as_str(), parsed.interactive), ("m", true));
}

#[test]
fn everything_after_the_dashes_belongs_to_the_command() {
    let parsed = run_args(&[
        "run",
        "--model",
        "m",
        "--",
        "sh",
        "-c",
        "echo --model x",
        "--",
        "--interactive",
    ]);
    assert_eq!(
        parsed.command,
        ["sh", "-c", "echo --model x", "--", "--interactive"]
    );
    assert_eq!(parsed.model, "m");
    assert!(!parsed.interactive);
}

#[test]
fn a_command_may_start_with_a_dash() {
    let parsed = run_args(&["run", "--model", "m", "--", "-weird"]);
    assert_eq!(parsed.command, ["-weird"]);
}

#[test]
fn a_note_may_start_with_dashes() {
    let parsed = run_args(&["run", "--model", "m", "--note", "--odd", "--", "c"]);
    assert_eq!(parsed.note.as_deref(), Some("--odd"));
}

#[test]
fn run_without_a_model_is_refused_by_name() {
    assert!(refused(&["run", "--", "true"]).contains("--model is required"));
}

#[test]
fn run_without_the_dashes_is_refused() {
    assert!(refused(&["run", "--model", "m"]).contains("the command goes after --"));
    assert!(refused(&["run", "--model", "m", "true"]).contains("does not understand true"));
}

#[test]
fn run_with_nothing_after_the_dashes_is_refused() {
    assert!(refused(&["run", "--model", "m", "--"]).contains("no command after --"));
}

#[test]
fn a_flag_missing_its_value_is_refused() {
    assert!(refused(&["run", "--model"]).contains("--model needs a value"));
    assert!(refused(&["run", "--model", "--", "true"]).contains("--model needs a value"));
    assert!(refused(&["run", "--model", "m", "--note"]).contains("--note needs a value"));
    assert!(refused(&["run", "--expected"]).contains("--expected needs a value"));
}

#[test]
fn an_unknown_flag_is_refused_by_name() {
    assert!(refused(&["run", "--model", "m", "--dry-run", "--", "c"]).contains("--dry-run"));
}

#[test]
fn an_expected_that_is_not_a_duration_is_refused() {
    let shown = refused(&["run", "--model", "m", "--expected", "soon", "--", "c"]);
    assert!(shown.contains("--expected"), "{shown}");
    for good in ["90", "500ms", "30s", "8h", "2m"] {
        run_args(&["run", "--model", "m", "--expected", good, "--", "c"]);
    }
}

#[test]
fn no_command_or_an_unknown_one_is_refused_with_the_usage() {
    for words in [&[][..], &["restart"][..], &["--help"][..]] {
        let shown = refused(words);
        assert!(shown.contains("shep paddock run --model"), "{shown}");
    }
}

fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn the_address_defaults_to_the_local_dog() {
    let link = Link::from_env(&env_of(&[("PADDOCK_KEY", "k")])).expect("a key is set");
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
    .expect("a key is set");
    assert_eq!(link.url, "http://gpu-host:8700");
}

#[test]
fn an_unset_or_empty_key_is_no_link() {
    assert!(Link::from_env(&env_of(&[])).is_none());
    assert!(Link::from_env(&env_of(&[("PADDOCK_KEY", "")])).is_none());
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

#[tokio::test]
async fn run_without_a_key_exits_2() {
    let parsed = Command::Run(run_args(&["run", "--model", "m", "--", "true"]));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(&env_of(&[]), parsed, &mut out, &mut err, &mut quiet()).await;
    assert_eq!(code, 2);
    assert!(String::from_utf8_lossy(&err).contains("PADDOCK_KEY"));
    assert!(out.is_empty());
}

#[tokio::test]
async fn status_without_a_key_exits_2_too() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(
        &env_of(&[]),
        Command::Status,
        &mut out,
        &mut err,
        &mut quiet(),
    )
    .await;
    assert_eq!(code, 2);
    assert!(String::from_utf8_lossy(&err).contains("PADDOCK_KEY"));
}

#[cfg(unix)]
#[tokio::test]
async fn term_and_hup_sent_to_this_process_arrive_as_forwards() {
    // Real signals, to this test process: the handlers are installed first, so neither kills it.
    let mut signals = forwarded_signals().expect("handlers install");
    let me = std::process::id().to_string();
    for (flag, expected) in [("-TERM", Forward::Terminate), ("-HUP", Forward::Hangup)] {
        let sent = std::process::Command::new("kill")
            .args([flag, &me])
            .status()
            .expect("kill runs");
        assert!(sent.success());
        let heard = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await;
        assert_eq!(heard, Ok(Some(expected)));
    }
}
