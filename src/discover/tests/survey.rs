//! What a model a survey finds in `/api/ps` counts as. No I/O.

use super::*;

const BASE: &str = "http://127.0.0.1:11434";

/// llama3 on one ollama named `ollama`, configured with no tag.
fn llama_untagged() -> Arc<Config> {
    config(&format!(
        r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "{BASE}"

[models.llama3]
backend = "ollama"
name = "llama3"
vram = "6G"
ram = "1G"
idle = "2h"
"#
    ))
}

fn listed(name: &str) -> OllamaLoaded {
    OllamaLoaded {
        name: name.to_owned(),
        footprint: Footprint {
            vram: Vram::Bytes(5 * GIB),
            ram: 0,
        },
        digest: None,
    }
}

#[test]
fn a_listed_model_the_config_names_without_a_tag_is_that_model() {
    let config = llama_untagged();
    let stray = ollama_stray(&config, &[], BASE, listed("llama3:latest")).expect("counted");
    assert_eq!(stray, config.models[&ModelName::from("llama3")]);
}

#[test]
fn a_listed_model_the_config_does_not_name_is_a_stand_in_past_the_names_taken() {
    let config = llama_untagged();
    let taken = [ModelName::from("ollama:qwen3.8:27b")];
    let stray = ollama_stray(&config, &taken, BASE, listed("qwen3.8:27b")).expect("counted");
    assert_eq!(stray.name, ModelName::from("ollama:ollama:qwen3.8:27b"));
    assert_eq!(stray.footprint, listed("qwen3.8:27b").footprint);
}

#[test]
fn nothing_counts_on_an_ollama_no_model_is_on() {
    let config = llama_untagged();
    assert_eq!(
        ollama_stray(&config, &[], "http://127.0.0.1:9", listed("llama3")),
        None
    );
}
