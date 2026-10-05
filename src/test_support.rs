//! Fakes and fixtures shared by the unit tests.

use std::sync::Arc;

use crate::config::Config;

/// Parses a config literal, for tests that need a `Config` and not its
/// validation. A bad literal is the test's bug, so it panics with the error.
pub(crate) fn config(toml: &str) -> Arc<Config> {
    match Config::from_toml(toml) {
        Ok(config) => Arc::new(config),
        Err(err) => panic!("test config does not parse: {err}"),
    }
}

/// The spec's example config with its real figures: the GPU host's totals,
/// the Strata models that each take all the VRAM, the ollama model beside
/// them, and laya, which holds only RAM. The book and engine tests build
/// their scenarios from it so they exercise the numbers the dog will run on.
pub(crate) const HOST_AND_MODELS: &str = r#"
listen = "0.0.0.0:8700"
grace = "2m"
max_wait = "120s"
reconnect = "60s"

[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "mac-sessions"
key = "k-mac"

[[clients]]
name = "bench-01"
key = "k-bench"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
apis = ["openai"]
vram = "22323M"
ram = "4G"
idle = "2h"

[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "37G"
idle = "2h"

[models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "44G"
idle = "2h"

[models.iq3_s]
backend = { sheep = "iq3_s" }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "55G"
excludes = ["laya"]
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "k-laya"
ready = { path = "/health", field = "loaded" }
ram = "5G"
idle = "8h"
"#;
