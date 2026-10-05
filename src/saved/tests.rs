use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use super::*;
use crate::book::Moment;

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

fn two_leases() -> Saved {
    Saved {
        version: VERSION,
        leases: vec![
            SavedLease {
                id: LeaseId(3),
                client: ClientName::from("bench-01"),
                model: ModelName::from("iq2_xs"),
                priority: Priority::Batch,
                since: at("2026-10-04T08:00:00Z"),
                expected_until: Some(at("2026-10-04T16:00:00Z")),
                note: Some("strata h2h run 3".to_owned()),
                hold: SavedHold::Connection {},
            },
            SavedLease {
                id: LeaseId(4),
                client: ClientName::from("mac-sessions"),
                model: ModelName::from("laya"),
                priority: Priority::Interactive,
                since: at("2026-10-04T09:30:00.250Z"),
                expected_until: None,
                note: None,
                hold: SavedHold::Heartbeat { ttl_ms: 60_000 },
            },
        ],
        sheep: BTreeMap::from([("iq2_xs".to_owned(), ModelName::from("iq2_xs"))]),
    }
}

fn logged(path: &Path) -> (Saved, String) {
    let mut log = Vec::new();
    let saved = load_or_empty(path, &mut log);
    (saved, String::from_utf8(log).expect("utf-8"))
}

#[test]
fn saved_state_round_trips() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = path_in(dir.path());

    store(&path, &two_leases()).expect("stored");

    assert_eq!(load(&path).expect("loaded"), Some(two_leases()));
}

#[test]
fn a_missing_file_is_none() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = path_in(dir.path());

    assert!(matches!(load(&path), Ok(None)));
    assert_eq!(logged(&path), (Saved::default(), String::new()));
}

#[test]
fn a_corrupt_file_starts_empty_and_says_why() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    std::fs::write(&path, "{\"version\": 1, \"leases\": [").expect("written");

    assert!(matches!(load(&path), Err(SavedError::Corrupt { .. })));
    let (saved, log) = logged(&path);

    assert_eq!(saved, Saved::default());
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(&path.display().to_string()), "{log}");
    assert!(log.contains("not valid saved state: EOF"), "{log}");
    let bad = dir.path().join("state.json.bad");
    assert!(
        log.contains(&format!("moved it to {}", bad.display())),
        "{log}"
    );
    assert!(!path.exists(), "the first save would overwrite it");
    assert_eq!(
        std::fs::read_to_string(&bad).expect("kept"),
        "{\"version\": 1, \"leases\": ["
    );
}

#[test]
fn a_bad_file_replaces_an_older_bad_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    let bad = dir.path().join("state.json.bad");
    std::fs::write(&bad, "older evidence").expect("written");
    std::fs::write(&path, "newer evidence").expect("written");

    let (saved, _) = logged(&path);

    assert_eq!(saved, Saved::default());
    assert_eq!(
        std::fs::read_to_string(&bad).expect("kept"),
        "newer evidence"
    );
}

#[test]
fn a_file_that_is_not_utf8_is_corrupt() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    std::fs::write(&path, [0xff, 0xfe, 0x00]).expect("written");

    assert!(matches!(load(&path), Err(SavedError::Corrupt { .. })));
}

#[test]
fn a_newer_version_starts_empty_and_says_why() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    let newer = json!({ "version": 2, "leases": "kept elsewhere" });
    std::fs::write(&path, newer.to_string()).expect("written");

    assert!(matches!(
        load(&path),
        Err(SavedError::Version { found: 2, .. })
    ));
    let (saved, log) = logged(&path);

    assert_eq!(saved, Saved::default());
    assert_eq!(
        log,
        format!(
            "paddock: {} is version 2, and this dog reads only version 1; \
             moved it to {}, starting with no saved leases\n",
            path.display(),
            dir.path().join("state.json.bad").display()
        )
    );
    let kept = std::fs::read_to_string(dir.path().join("state.json.bad")).expect("kept");
    assert_eq!(kept, newer.to_string(), "a newer dog's leases are kept");
    assert!(!path.exists());
}

#[test]
fn a_file_with_no_version_is_corrupt() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    std::fs::write(&path, r#"{"leases": [], "sheep": {}}"#).expect("written");

    assert!(matches!(load(&path), Err(SavedError::Corrupt { .. })));
}

/// The version 1 bytes as written to disk: they must keep reading as the same value.
#[test]
fn a_version_1_file_reads() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("state.json");
    let v1 = r#"{
  "version": 1,
  "leases": [
    {
      "id": 3,
      "client": "bench-01",
      "model": "iq2_xs",
      "priority": "batch",
      "since": "2026-10-04T08:00:00Z",
      "expected_until": "2026-10-04T16:00:00Z",
      "note": "strata h2h run 3",
      "hold": { "connection": {} }
    },
    {
      "id": 4,
      "client": "mac-sessions",
      "model": "laya",
      "priority": "interactive",
      "since": "2026-10-04T09:30:00.25Z",
      "expected_until": null,
      "note": null,
      "hold": { "heartbeat": { "ttl_ms": 60000 } }
    }
  ],
  "sheep": { "iq2_xs": "iq2_xs" }
}"#;
    std::fs::write(&path, v1).expect("written");

    assert_eq!(load(&path).expect("loaded"), Some(two_leases()));
}

#[test]
fn a_hold_is_written_as_the_spec_says() {
    let written = |hold: Hold| serde_json::to_value(SavedHold::from(hold)).expect("json");

    assert_eq!(written(Hold::Connection), json!({ "connection": {} }));
    assert_eq!(
        written(Hold::Heartbeat {
            ttl: Duration::from_secs(60)
        }),
        json!({ "heartbeat": { "ttl_ms": 60000 } })
    );
}

#[test]
fn the_file_lives_under_shep_home_paddock() {
    assert_eq!(
        path_in(Path::new("/home/shep")),
        Path::new("/home/shep/paddock/state.json")
    );
}

#[test]
fn storing_creates_the_paddock_directory_and_leaves_no_staging_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = path_in(dir.path());

    store(&path, &Saved::default()).expect("stored");

    let names: Vec<_> = std::fs::read_dir(dir.path().join("paddock"))
        .expect("the directory exists")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(names, ["state.json"]);
}

#[test]
fn a_store_that_cannot_write_says_where() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let blocker = dir.path().join("paddock");
    std::fs::write(&blocker, "a file where the directory goes").expect("written");
    let path = path_in(dir.path());

    let err = store(&path, &Saved::default()).expect_err("cannot write");

    assert!(matches!(&err, SavedError::Write { path: at, .. } if *at == path));
}

/// R20: saved times map to the clock's moments, so a grant a month old keeps its age.
#[tokio::test(start_paused = true)]
async fn a_restored_lease_keeps_its_times_through_the_clock() {
    let clock = Clock::new();
    let now = clock.wall(clock.moment());
    let lease = SavedLease {
        since: now - SignedDuration::from_hours(30 * 24),
        expected_until: Some(now + SignedDuration::from_hours(2)),
        ..two_leases().leases.remove(0)
    };

    let restored = lease.clone().restored(&clock);

    assert_eq!(restored.since, clock.moment_of(lease.since));
    let until = restored
        .ask
        .expected
        .map(|expected| restored.since.plus(expected));
    assert_eq!(until, lease.expected_until.map(|at| clock.moment_of(at)));
    assert_eq!(restored.ask.hold, Hold::Connection);
    assert_eq!(restored.ask.max_wait, None);
}

/// A grant older than the clock reaches saturates at Moment(0), and still ends when it said.
#[tokio::test(start_paused = true)]
async fn a_grant_older_than_a_year_keeps_its_expected_end() {
    let clock = Clock::new();
    let now = clock.wall(clock.moment());
    let lease = SavedLease {
        since: now - SignedDuration::from_hours(400 * 24),
        expected_until: Some(now + SignedDuration::from_hours(2)),
        ..two_leases().leases.remove(0)
    };
    let until = lease.expected_until.map(|at| clock.moment_of(at));

    let restored = lease.restored(&clock);

    assert_eq!(restored.since, Moment(0));
    assert_eq!(
        restored
            .ask
            .expected
            .map(|expected| restored.since.plus(expected)),
        until
    );
}

#[tokio::test(start_paused = true)]
async fn a_saved_lease_reads_back_as_the_view_it_came_from() {
    let clock = Clock::new();
    let moment = clock.moment();
    let view = LeaseView {
        id: LeaseId(9),
        client: ClientName::from("bench-01"),
        model: ModelName::from("iq3_s"),
        priority: Priority::Interactive,
        since: Moment(moment.0 - 5_000),
        expected_until: Some(Moment(moment.0 + 3_600_000)),
        note: Some("sweep".to_owned()),
        hold: Hold::Heartbeat {
            ttl: Duration::from_secs(30),
        },
        attached: true,
    };

    let restored = SavedLease::from_view(view.clone(), &clock).restored(&clock);

    assert_eq!(restored.since, view.since);
    assert_eq!(restored.ask.lease, view.id);
    assert_eq!(restored.ask.client, view.client);
    assert_eq!(restored.ask.model, view.model);
    assert_eq!(restored.ask.priority, view.priority);
    assert_eq!(restored.ask.hold, view.hold);
    assert_eq!(restored.ask.note, view.note);
    assert_eq!(
        restored.ask.expected,
        Some(Duration::from_millis(3_605_000))
    );
}
