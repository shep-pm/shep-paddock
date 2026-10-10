//! One model's section, read and checked into a [`Model`].

use std::collections::BTreeMap;

use reqwest::Url;

use super::{
    Backend, ConfigError, DEFAULT_LOAD_TIMEOUT, Model, ModelName, Ready,
    section::{self, BackendKind, BackendRef, ModelSection},
    values::{duration_or, parse_duration, parse_ram, parse_vram},
};
use crate::footprint::{Footprint, Host};

/// A url without trailing slashes, so a path appended to it has one slash
pub(super) fn trim_slashes(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

/// `url` as a base to forward to, when it is an http or https url with a host
fn parse_base(url: &str) -> Option<Url> {
    Url::parse(url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

pub(super) fn build_model(
    name: &ModelName,
    raw: ModelSection,
    backends: &BTreeMap<String, section::BackendSection>,
    host: &Host,
) -> Result<Model, ConfigError> {
    let field = |leaf: &str| format!("models.{name}.{leaf}");
    let bad_url = || ConfigError::BadUrl {
        model: name.clone(),
    };
    let (backend, url, base) = match raw.backend {
        BackendRef::Named(backend) => {
            let named = backends
                .get(&backend)
                .ok_or_else(|| ConfigError::UnknownBackend {
                    model: name.clone(),
                    backend,
                })?;
            match named.kind {
                BackendKind::Ollama => {
                    let model_name = raw.name.ok_or_else(|| ConfigError::MissingName {
                        model: name.clone(),
                    })?;
                    let backend_url = trim_slashes(&named.url);
                    let base = parse_base(&backend_url).ok_or_else(bad_url)?;
                    let url = raw.url.as_deref().map_or(backend_url.clone(), trim_slashes);
                    parse_base(&url).ok_or_else(bad_url)?;
                    (
                        Backend::Ollama {
                            url: backend_url,
                            name: model_name,
                        },
                        Some(url),
                        Some(base),
                    )
                }
            }
        }
        BackendRef::Sheep(sheep) => {
            if raw.url.is_none() {
                return Err(ConfigError::MissingUrl {
                    model: name.clone(),
                });
            }
            let url = raw.url.as_deref().map(trim_slashes);
            let base = url
                .as_deref()
                .map(|url| parse_base(url).ok_or_else(bad_url))
                .transpose()?;
            (
                Backend::Sheep {
                    sheep: sheep.sheep,
                    name: raw.name,
                    script: None,
                    args: sheep.args,
                    env: sheep.env,
                },
                url,
                base,
            )
        }
    };

    if !raw.placements.is_empty() {
        if matches!(backend, Backend::Ollama { .. }) {
            return Err(ConfigError::PlacementsOnOllama {
                model: name.clone(),
            });
        }
        if raw.vram.is_some() || raw.ram.is_some() {
            return Err(ConfigError::FootprintBesidePlacements {
                model: name.clone(),
            });
        }
    }
    if let Some(container) = &raw.container {
        if matches!(backend, Backend::Ollama { .. }) {
            return Err(ConfigError::ContainerOnOllama {
                model: name.clone(),
            });
        }
        if !podman_name(container) {
            return Err(ConfigError::BadContainer {
                model: name.clone(),
                container: container.clone(),
            });
        }
    }
    let placements = super::placement::build(name, raw.placements, host)?;
    let footprint = match placements.split_first() {
        Some((first, rest)) => rest
            .iter()
            .fold(first.footprint, |larger, p| larger.larger(p.footprint)),
        None => {
            let footprint = Footprint {
                vram: parse_vram(raw.vram.as_deref(), &field("vram"))?,
                ram: parse_ram(raw.ram.as_deref(), &field("ram"))?,
            };
            if !host.ever_fits(&footprint) {
                return Err(ConfigError::NeverFits {
                    model: name.clone(),
                });
            }
            footprint
        }
    };

    Ok(Model {
        name: name.clone(),
        backend,
        url,
        base,
        ready: raw.ready.map(|ready| Ready {
            path: ready.path,
            field: ready.field,
        }),
        apis: raw.apis,
        prefix: raw.prefix,
        key: raw.key,
        footprint,
        placements,
        excludes: raw.excludes.into_iter().map(ModelName::from).collect(),
        idle: parse_duration(&raw.idle, &field("idle"))?,
        load_timeout: duration_or(
            raw.load_timeout.as_deref(),
            &field("load_timeout"),
            DEFAULT_LOAD_TIMEOUT,
        )?,
        sequences: raw.sequences,
        container: raw.container,
    })
}

/// Whether `name` is one podman gives a container: a letter or digit, then letters, digits, `_`,
/// `.` or `-`, so it can never be read as a flag
fn podman_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[cfg(test)]
mod tests;
