//! A sheep the saved state records for a model a reload has since moved to another sheep.
//!
//! Real time, like the other discovery tests, and every await is bounded by `LIMIT`.

use super::*;

/// laya now on the laya-2 sheep, its ready check at `base`.
fn moved_laya(base: &str) -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[models.laya]
backend = {{ sheep = "laya-2" }}
url = "{base}"
ready = {{ path = "/health", field = "loaded" }}
ram = "5G"
idle = "8h"
"#
    ))
}

fn laya_ram() -> Footprint {
    Footprint {
        vram: Vram::None,
        ram: 5 * GIB,
    }
}

/// The ready check asks laya's new backend, which is not up, so it must not decide.
#[tokio::test]
async fn a_running_sheep_recorded_for_a_moved_model_counts_as_that_model_there() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, http) = fake_http(vec![("GET", "/health", vec![(503, "down")])]);
    let saved = saved_in(home.path(), &[("laya", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");

    let discovered = found(&moved_laya(&base), shepherd, &saved).await;

    assert_eq!(discovered.loaded, [counted("laya", laya_ram(), false)]);
    let [on_old] = discovered.stand_ins.as_slice() else {
        panic!("one model to unload by: {:?}", discovered.stand_ins);
    };
    assert_eq!(on_old.name, ModelName::from("laya"));
    assert_eq!(on_old.backend.sheep(), Some("laya"));
    assert!(http.seen().is_empty(), "no ready check is asked");
}

#[tokio::test]
async fn a_recorded_sheep_that_is_not_running_counts_nothing() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(Vec::new());
    let saved = saved_in(home.path(), &[("laya", "laya")]);

    let discovered = found(&moved_laya(&base), FakeShepherd::new(), &saved).await;

    assert_eq!(discovered, Discovered::default());
}

#[tokio::test]
async fn a_moved_model_found_on_its_new_sheep_counts_its_old_one_as_a_stand_in() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (base, _http) = fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":true}"#)])]);
    let saved = saved_in(home.path(), &[("laya", "laya"), ("laya-2", "laya")]);
    let shepherd = FakeShepherd::new();
    shepherd.running("laya");
    shepherd.running("laya-2");

    let discovered = found(&moved_laya(&base), shepherd, &saved).await;

    assert_eq!(
        discovered.loaded,
        [
            counted("laya", laya_ram(), false),
            counted("sheep:laya", laya_ram(), false)
        ]
    );
    let [on_old] = discovered.stand_ins.as_slice() else {
        panic!("one stand-in: {:?}", discovered.stand_ins);
    };
    assert_eq!(on_old.name, ModelName::from("sheep:laya"));
    assert_eq!(on_old.backend.sheep(), Some("laya"));
}
