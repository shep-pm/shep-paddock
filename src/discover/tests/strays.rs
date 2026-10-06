//! What a restart finds on a sheep with no record, and the placements it restores.
//!
//! Real time, like the other discovery tests. The ready checks go to a fake server on a real
//! socket, and every await is bounded by `LIMIT`.

use super::*;
use crate::{config::PlacementName, test_support::LAYA_PLACEMENTS};

/// laya alone on its sheep, with its ready check at `base`, declared by `body`.
fn lone_laya(base: &str, body: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
{body}"#
    ))
}

const PLAIN: &str = "ram = \"5G\"\nidle = \"8h\"\n";

fn laya_found(footprint: Footprint, placement: Option<&str>, stray: bool) -> Found {
    Found {
        model: ModelName::from("laya"),
        footprint,
        placement: placement.map(PlacementName::from),
        stray,
    }
}

fn largest() -> Footprint {
    Footprint {
        vram: Vram::Bytes(6 * GIB),
        ram: 5 * GIB,
    }
}

fn in_ram() -> Footprint {
    Footprint {
        vram: Vram::None,
        ram: 5 * GIB,
    }
}

/// The saved state for laya on its sheep, with `models` as written.
fn laya_saved(home: &Path, placement: Option<&str>, stray: bool) -> Saved {
    let mut saved = saved_in(home, &[("laya", "laya")]);
    saved.models.insert(
        ModelName::from("laya"),
        SavedModel {
            placement: placement.map(PlacementName::from),
            stray,
        },
    );
    saved
}

#[tokio::test]
async fn a_lone_model_on_a_running_sheep_with_no_record_is_that_model_as_a_stray() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(
        &lone_laya(&base, PLAIN),
        shepherd,
        &saved_in(home.path(), &[]),
    )
    .await;

    assert_eq!(discovered.loaded, [laya_found(in_ram(), None, true)]);
    assert!(discovered.stand_ins.is_empty());
}

#[tokio::test]
async fn a_lone_model_with_placements_and_no_record_counts_at_its_largest() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let config = lone_laya(&base, LAYA_PLACEMENTS);
    let discovered = found(&config, shepherd, &saved_in(home.path(), &[])).await;

    assert_eq!(discovered.loaded, [laya_found(largest(), None, true)]);
}

#[tokio::test]
async fn a_lone_model_that_is_not_ready_is_a_stand_in() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(503, "loading")])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(
        &lone_laya(&base, PLAIN),
        shepherd,
        &saved_in(home.path(), &[]),
    )
    .await;

    assert_eq!(stand_ins(&discovered), ["sheep:laya"]);
    let names: Vec<_> = discovered
        .loaded
        .iter()
        .map(|found| (found.model.as_str(), found.footprint, found.stray))
        .collect();
    assert_eq!(names, [("sheep:laya", in_ram(), true)]);
}

#[tokio::test]
async fn a_saved_placement_is_restored_at_its_figures() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let saved = laya_saved(home.path(), Some("ram"), false);
    let discovered = found(&lone_laya(&base, LAYA_PLACEMENTS), shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [laya_found(in_ram(), Some("ram"), false)]
    );
}

#[tokio::test]
async fn a_saved_placement_the_config_no_longer_declares_counts_at_the_largest() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let saved = laya_saved(home.path(), Some("cpu"), false);
    let discovered = found(&lone_laya(&base, LAYA_PLACEMENTS), shepherd, &saved).await;

    assert_eq!(discovered.loaded, [laya_found(largest(), None, false)]);
}

#[tokio::test]
async fn a_model_saved_as_a_stray_is_still_one() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let saved = laya_saved(home.path(), None, true);
    let discovered = found(&lone_laya(&base, PLAIN), shepherd, &saved).await;

    assert_eq!(discovered.loaded, [laya_found(in_ram(), None, true)]);
}

/// A version 2 file names every model holding memory, so a sheep record with no `models` entry
/// is a sheep the dog stopped and something else started again.
#[tokio::test]
async fn a_sheep_record_without_a_models_entry_is_a_stray_only_in_a_version_2_file() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let config = lone_laya(&base, PLAIN);
    let mut saved = saved_in(home.path(), &[("laya", "laya")]);
    saved.models.clear();

    let shepherd = FakeShepherd::new();
    shepherd.running("laya");
    let discovered = found(&config, shepherd, &saved).await;
    assert_eq!(
        discovered.loaded,
        [laya_found(in_ram(), None, true)],
        "version 2"
    );

    let shepherd = FakeShepherd::new();
    shepherd.running("laya");
    let version_1 = Saved {
        version: 1,
        ..saved
    };
    let discovered = found(&config, shepherd, &version_1).await;
    assert_eq!(
        discovered.loaded,
        [laya_found(in_ram(), None, false)],
        "version 1 has no models to say otherwise"
    );
}

#[test]
fn unrecorded_is_the_one_model_a_stand_in_for_several_and_none_for_no_model() {
    let config = config(crate::test_support::HOST_AND_MODELS);
    let name = |sheep| unrecorded(&config, sheep).map(|model| model.name);
    assert_eq!(name("laya"), Some(ModelName::from("laya")));
    assert_eq!(name("iq2_xs"), Some(ModelName::from("sheep:iq2_xs")));
    assert_eq!(name("postgres"), None);
}

/// A hand-edited file can place a model whose sheep has no record. The dog never writes that.
#[tokio::test]
async fn a_placement_saved_without_a_sheep_record_is_not_restored() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let mut saved = laya_saved(home.path(), Some("ram"), false);
    saved.sheep.clear();
    let discovered = found(&lone_laya(&base, LAYA_PLACEMENTS), shepherd, &saved).await;

    assert_eq!(discovered.loaded, [laya_found(largest(), None, true)]);
}
