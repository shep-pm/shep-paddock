use std::time::Duration;

use super::{Command, Forward, RunArgs, execute, forwarded_signals, parse};

mod bare;
mod stored_key;

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
            model: Some("iq2_xs".to_owned()),
            vram: None,
            ram: None,
            grace: super::STOP_GRACE,
            expected: Some("8h".to_owned()),
            note: Some("strata run 3".to_owned()),
            interactive: true,
            release_if_idle: None,
            reclaimable: false,
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
    assert_eq!(
        (parsed.model.as_deref(), parsed.interactive),
        (Some("m"), true)
    );
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
    assert_eq!(parsed.model.as_deref(), Some("m"));
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
fn a_repeated_flag_is_refused_by_name() {
    for (flag, value) in [
        ("--model", Some("m2")),
        ("--note", Some("again")),
        ("--expected", Some("2h")),
        ("--interactive", None),
    ] {
        let mut words = vec!["run", "--model", "m", "--note", "n", "--expected", "1h"];
        words.push("--interactive");
        words.push(flag);
        words.extend(value);
        words.extend(["--", "c"]);
        let shown = refused(&words);
        assert!(
            shown.contains(&format!("{flag} given more than once")),
            "{shown}"
        );
    }
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

fn note_env(url: String, lease: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
    move |name| match name {
        "PADDOCK_KEY" => Some("k-bench".to_owned()),
        "PADDOCK_URL" => Some(url.clone()),
        "PADDOCK_LEASE" => lease.map(str::to_owned),
        _ => None,
    }
}

// Real time, because the fake dog is a real loopback socket; the await is bounded.
#[tokio::test]
async fn note_goes_to_the_lease_in_paddock_lease() {
    let (url, dog) =
        crate::test_support::fake_http(vec![("PUT", "/paddock/leases/L9", vec![(204, "")])]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (env, mut signals) = (note_env(url, Some("L9")), quiet());
    let sent = execute(
        &env,
        Command::Note("step 1".to_owned()),
        &mut out,
        &mut err,
        &mut signals,
    );
    let code = tokio::time::timeout(Duration::from_secs(10), sent)
        .await
        .expect("finishes");
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let seen = dog.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer k-bench"));
    assert_eq!(seen[0].body, r#"{"note":"step 1"}"#);
}

#[tokio::test]
async fn note_without_paddock_lease_exits_2_and_sends_nothing() {
    let (url, dog) = crate::test_support::fake_http(vec![]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (env, mut signals) = (note_env(url, None), quiet());
    let sent = execute(
        &env,
        Command::Note("step 1".to_owned()),
        &mut out,
        &mut err,
        &mut signals,
    );
    let code = tokio::time::timeout(Duration::from_secs(10), sent)
        .await
        .expect("finishes");
    assert_eq!(code, 2);
    assert!(dog.seen().is_empty());
}

/// Set in the environment of the child that [`forwards_are_heard_in_the_child`] runs as.
#[cfg(unix)]
const CHILD: &str = "PADDOCK_TEST_SIGNAL_CHILD";

/// The child half of the signal test: installs the handlers and reports each forward it hears
///
/// Does nothing in an ordinary run. The handlers it installs live as long as its process, so
/// the test process of a normal run never has them.
#[cfg(unix)]
#[tokio::test]
async fn forwards_are_heard_in_the_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let mut signals = forwarded_signals().expect("handlers install");
    println!("child: ready");
    for _ in 0..3 {
        let heard = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await;
        println!("child: heard {heard:?}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn int_term_and_hup_sent_to_a_process_arrive_as_forwards() {
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    // A subprocess, so the handlers it installs die with it and cannot make this test process
    // ignore a TERM that should stop a hung run.
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "cli::tests::forwards_are_heard_in_the_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("child starts");
    let pid = child.id().expect("child pid").to_string();
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut next = async || {
        loop {
            let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
                .await
                .expect("the child speaks in time")
                .expect("the child's output reads")
                .expect("the child has not exited");
            // libtest starts the first line with the test's name.
            if let Some((_, said)) = line.split_once("child: ") {
                return said.to_owned();
            }
        }
    };
    assert_eq!(next().await, "ready");
    for (flag, expected) in [
        ("-INT", Forward::Interrupt),
        ("-TERM", Forward::Terminate),
        ("-HUP", Forward::Hangup),
    ] {
        let sent = std::process::Command::new("kill")
            .args([flag, &pid])
            .status()
            .expect("kill runs");
        assert!(sent.success());
        assert_eq!(next().await, format!("heard Ok(Some({expected:?}))"));
    }
    let exited = tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("the child exits")
        .expect("wait");
    assert!(exited.success(), "{exited}");
}

#[test]
fn run_reads_release_if_idle_and_reclaimable() {
    let parsed = run_args(&[
        "run",
        "--model",
        "qwen",
        "--release-if-idle",
        "30m",
        "--reclaimable",
        "--",
        "bench",
    ]);
    assert_eq!(parsed.release_if_idle.as_deref(), Some("30m"));
    assert!(parsed.reclaimable);
}

#[test]
fn a_release_if_idle_that_is_not_a_duration_is_refused() {
    let said = refused(&[
        "run",
        "--model",
        "qwen",
        "--release-if-idle",
        "half",
        "--",
        "bench",
    ]);
    assert!(
        said.contains("--release-if-idle is not a duration such as 30s or 8h: half."),
        "{said}"
    );
}

#[test]
fn a_release_if_idle_of_zero_is_refused() {
    for zero in ["0", "0s", "0ms"] {
        let said = refused(&[
            "run",
            "--model",
            "qwen",
            "--release-if-idle",
            zero,
            "--",
            "bench",
        ]);
        assert!(
            said.contains(&format!("--release-if-idle must be more than 0: {zero}.")),
            "{said}"
        );
    }
}

#[test]
fn reclaimable_given_twice_is_refused() {
    let said = refused(&[
        "run",
        "--model",
        "qwen",
        "--reclaimable",
        "--reclaimable",
        "--",
        "bench",
    ]);
    assert!(
        said.contains("--reclaimable given more than once."),
        "{said}"
    );
}

#[test]
fn note_takes_its_text_as_one_argument() {
    assert_eq!(
        args(&["note", "step 412/900"]),
        Ok(Command::Note("step 412/900".to_owned()))
    );
    for words in [&["note"][..], &["note", "step", "412"][..]] {
        let said = refused(words);
        assert!(
            said.contains("note takes the text to send, as one argument."),
            "{said}"
        );
    }
}

#[test]
fn plain_escapes_control_and_bidi_characters_and_leaves_other_text_alone() {
    use super::plain;
    assert_eq!(
        plain("a\u{1b}[2Jb\u{7f}\u{80}\u{9b}\u{9f}\n"),
        "a\\u{1b}[2Jb\\u{7f}\\u{80}\\u{9b}\\u{9f}\\u{a}"
    );
    for bidi in [
        '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{61c}',
    ] {
        let said = plain(&format!("x{bidi}y"));
        assert!(!said.contains(bidi), "{said:?}");
        assert!(
            said.starts_with("x\\u{") && said.ends_with("}y"),
            "{said:?}"
        );
    }
    assert_eq!(
        plain("caf\u{e9} \u{65e5}\u{672c} \u{1f411}"),
        "caf\u{e9} \u{65e5}\u{672c} \u{1f411}"
    );
}
