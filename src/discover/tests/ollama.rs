//! Discovery of ollama models through `/api/ps`, on real time like the sheep tests.

use super::*;

#[tokio::test]
async fn an_ollama_model_in_api_ps_is_loaded() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[{"name":"qwen3.8:27b-ctx131072","model":"qwen3.8:27b-ctx131072","size":26000000000}]}"#;
    let (base, http) = fake_http(vec![("GET", "/api/ps", vec![(200, ps)])]);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
vram = "22323M"
ram = "4G"
idle = "2h"

[models.small]
backend = "ollama"
name = "llama3:8b"
vram = "6G"
idle = "2h"
"#
    ));
    let saved = saved_in(home.path(), &[]);

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    assert_eq!(
        discovered.loaded,
        [(
            ModelName::from("qwen3.8:27b"),
            Footprint {
                vram: Vram::Bytes(22_323 * MIB),
                ram: 4 * GIB,
            },
        )]
    );
    assert!(discovered.stand_ins.is_empty());
    assert_eq!(http.seen().len(), 1, "one /api/ps for the one ollama");
}

#[tokio::test]
async fn an_ollama_that_does_not_answer_has_nothing_loaded() {
    let home = tempfile::TempDir::new().expect("tempdir");
    // Bound, then dropped, so the port refuses the connection.
    let (base, http) = fake_http(Vec::new());
    drop(http);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ));
    let saved = saved_in(home.path(), &[]);

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    assert_eq!(discovered, Discovered::default());
}

/// One ollama with `models` configured, as `(config name, ollama name, vram)`.
fn ollama_with(base: &str, models: &[(&str, &str, &str)]) -> Arc<Config> {
    let mut text = format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"
"#
    );
    for (model, name, vram) in models {
        text.push_str(&format!(
            "\n[models.\"{model}\"]\nbackend = \"ollama\"\nname = \"{name}\"\nvram = \"{vram}\"\nidle = \"2h\"\n"
        ));
    }
    config(&text)
}

/// Memory ollama holds for a model the config does not name still counts.
#[tokio::test]
async fn an_unconfigured_model_in_api_ps_is_unknown_at_its_reported_figures() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[
        {"name":"qwen3.8:27b-ctx131072","size":26000000000,"size_vram":23000000000},
        {"name":"llama3:8b","size":6000000000,"size_vram":5000000000},
        {"name":"tiny:1b","size":100,"size_vram":400}
    ]}"#;
    let (base, _http) = fake_http(vec![("GET", "/api/ps", vec![(200, ps)])]);
    let config = ollama_with(&base, &[("qwen3.8:27b", "qwen3.8:27b-ctx131072", "22323M")]);
    let saved = saved_in(home.path(), &[]);

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    assert_eq!(
        discovered.loaded,
        [
            (
                ModelName::from("qwen3.8:27b"),
                Footprint {
                    vram: Vram::Bytes(22_323 * MIB),
                    ram: 0,
                },
            ),
            (
                ModelName::from("ollama:llama3:8b"),
                Footprint {
                    vram: Vram::Bytes(5_000_000_000),
                    ram: 1_000_000_000,
                },
            ),
            (
                ModelName::from("ollama:tiny:1b"),
                Footprint {
                    vram: Vram::Bytes(400),
                    ram: 0,
                },
            ),
        ]
    );
    let stand_in = &discovered.stand_ins[0];
    assert_eq!(stand_in.name, ModelName::from("ollama:llama3:8b"));
    assert_eq!(
        stand_in.backend,
        Backend::Ollama {
            url: base.clone(),
            name: "llama3:8b".to_owned(),
        }
    );
    assert_eq!(
        stand_ins(&discovered),
        ["ollama:llama3:8b", "ollama:tiny:1b"]
    );
}

#[tokio::test]
async fn an_untagged_name_matches_latest_on_either_side() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[{"name":"qwen:latest","size":9},{"name":"mistral","size":9}]}"#;
    let (base, _http) = fake_http(vec![("GET", "/api/ps", vec![(200, ps)])]);
    let config = ollama_with(
        &base,
        &[("qwen", "qwen", "4G"), ("mistral", "mistral:latest", "5G")],
    );
    let saved = saved_in(home.path(), &[]);

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [&ModelName::from("mistral"), &ModelName::from("qwen")]
    );
    assert!(discovered.stand_ins.is_empty());
}

#[tokio::test]
async fn an_unknown_ollama_model_is_never_named_as_a_configured_model() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[{"name":"llama3:8b","size":9}]}"#;
    let (base, _http) = fake_http(vec![("GET", "/api/ps", vec![(200, ps)])]);
    let config = ollama_with(&base, &[("ollama:llama3:8b", "other:1b", "1G")]);
    let saved = saved_in(home.path(), &[]);

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|(name, _)| name).collect();
    assert_eq!(names, [&ModelName::from("ollama:ollama:llama3:8b")]);
}

/// A lease must keep its ollama model across a restart, as it keeps a sheep's.
#[tokio::test]
async fn a_leased_ollama_model_in_api_ps_is_that_model_when_not_ready() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let ps = r#"{"models":[{"name":"qwen3.8:27b-ctx131072","size":26000000000}]}"#;
    let (base, http) = fake_http(vec![
        ("GET", "/api/ps", vec![(200, ps)]),
        ("GET", "/health", vec![(503, "loading")]),
    ]);
    let config = config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
ready = {{ path = "/health", field = "loaded" }}
vram = "22323M"
ram = "4G"
idle = "2h"
"#
    ));
    let mut saved = saved_in(home.path(), &[]);
    saved.leases.push(SavedLease {
        id: LeaseId(5),
        client: "bench-01".into(),
        model: "qwen3.8:27b".into(),
        priority: Priority::Batch,
        since: jiff::Timestamp::now(),
        expected_until: None,
        note: None,
        hold: SavedHold::Connection {},
    });

    let discovered = found(&config, FakeShepherd::new(), &saved).await;

    let names: Vec<_> = discovered.loaded.iter().map(|(name, _)| name).collect();
    assert_eq!(names, [&ModelName::from("qwen3.8:27b")]);
    assert!(
        discovered.stand_ins.is_empty(),
        "{:?}",
        discovered.stand_ins
    );
    assert!(
        http.seen().iter().all(|seen| seen.path != "/health"),
        "the ready check is not asked"
    );
}
