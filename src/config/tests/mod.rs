mod clients;
mod errors;
mod models;
mod ollama;
mod redaction;
mod sheep;
mod values;

use super::*;

pub(super) const MINIMAL: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[[clients]]
name = "bench-01"
key = "k-bench"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
ram = "5G"
idle = "8h"
"#;

pub(super) fn name(text: &str) -> ModelName {
    ModelName::from(text)
}

/// MINIMAL plus a second sheep model, `iq3_s`, that holds all the VRAM.
pub(super) fn with_iq3_s(extra: &str) -> String {
    format!(
        "{MINIMAL}
[models.iq3_s]
backend = {{ sheep = \"iq3_s\" }}
url = \"http://127.0.0.1:8080\"
vram = \"all\"
ram = \"20G\"
idle = \"2h\"
{extra}
"
    )
}

/// Two ollama models on `backends.ollama`, named `first` and `second` to ollama.
pub(super) fn two_ollama_models(first: &str, second: &str) -> String {
    format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[backends.other]
kind = "ollama"
url = "http://127.0.0.1:11435"

[models.q]
backend = "ollama"
name = "{first}"
vram = "10G"
idle = "2h"

[models.r]
backend = "{second}"
name = "qwen3"
vram = "10G"
idle = "2h"
"#
    )
}

pub(super) fn name_of(text: &str) -> ClientName {
    ClientName::from(text)
}
