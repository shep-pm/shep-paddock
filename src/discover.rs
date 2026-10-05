//! Finding what is loaded when the dog starts, before it listens.
//!
//! A sheep that a configured model runs on, and that the flock shows running,
//! serves the model the saved state names for it once that model's ready check
//! passes. Otherwise it was started outside the dog, or is not ready to say,
//! so it counts as unknown at the largest footprint of the models on it. An
//! ollama model is loaded when `/api/ps` lists its name and its ready check
//! passes.

use std::collections::{BTreeMap, BTreeSet};

use shep_client::shep_core::status::ProcStatus;

use crate::{
    backend::Backends,
    config::{Backend, Config, Model, ModelName},
    footprint::Footprint,
    saved::Saved,
    shepherd::Shepherd,
};

/// What discovery found holding memory
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Discovered {
    /// Each model found loaded, unknown ones under their [`stand_in`] names.
    pub loaded: Vec<(ModelName, Footprint)>,
    /// The running sheep whose model is not known, by sheep name.
    pub unknown: Vec<String>,
}

/// Finds every model loaded on the host, asking each backend once
///
/// A shepherd or an ollama that does not answer is logged, and nothing on it counts.
pub(crate) async fn discover<S: Shepherd>(
    config: &Config,
    backends: &Backends<S>,
    saved: &Saved,
) -> Discovered {
    let mut found = Discovered::default();
    let running: BTreeSet<String> = match backends.shepherd().list_flock().await {
        Ok(flock) => flock
            .into_iter()
            .filter(|row| matches!(row.status, ProcStatus::Starting | ProcStatus::Online))
            .map(|row| row.name)
            .collect(),
        Err(err) => {
            eprintln!("paddock: listing the flock at start failed, so no sheep counts: {err}");
            BTreeSet::new()
        }
    };
    for (sheep, models) in by_sheep(config) {
        if !running.contains(sheep) {
            continue;
        }
        let named = saved
            .sheep
            .get(sheep)
            .and_then(|name| models.iter().find(|model| model.name == *name));
        match named {
            Some(model) if backends.ready_now(model).await => {
                found.loaded.push((model.name.clone(), model.footprint));
            }
            _ => found.unknown(config, sheep),
        }
    }
    for (url, models) in by_ollama(config) {
        let key = models.iter().find_map(|model| model.key());
        let names = match backends.ollama_loaded(url, key).await {
            Ok(names) => names,
            Err(err) => {
                eprintln!("paddock: asking ollama what it has loaded failed: {err}");
                continue;
            }
        };
        for model in models {
            let Backend::Ollama { name, .. } = &model.backend else {
                continue;
            };
            if names.contains(name) && backends.ready_now(model).await {
                found.loaded.push((model.name.clone(), model.footprint));
            }
        }
    }
    found
}

impl Discovered {
    /// Counts `sheep` as running a model nobody named
    fn unknown(&mut self, config: &Config, sheep: &str) {
        if let Some(model) = stand_in(config, sheep) {
            self.loaded.push((model.name, model.footprint));
            self.unknown.push(sheep.to_owned());
        }
    }
}

/// The configured models on each ollama, by its url
fn by_ollama(config: &Config) -> BTreeMap<&str, Vec<&Model>> {
    let mut on: BTreeMap<&str, Vec<&Model>> = BTreeMap::new();
    for model in config.models.values() {
        if let Backend::Ollama { url, .. } = &model.backend {
            on.entry(url.as_str()).or_default().push(model);
        }
    }
    on
}

/// The configured models on each sheep, by sheep name
fn by_sheep(config: &Config) -> BTreeMap<&str, Vec<&Model>> {
    let mut on: BTreeMap<&str, Vec<&Model>> = BTreeMap::new();
    for model in config.models.values() {
        if let Backend::Sheep { sheep, .. } = &model.backend {
            on.entry(sheep.as_str()).or_default().push(model);
        }
    }
    on
}

/// The model an unknown sheep counts as: named for no configured model, at the
/// larger figures in each resource of every model on that sheep
///
/// Unloading it stops the sheep. `None` when no configured model runs on `sheep`.
pub(crate) fn stand_in(config: &Config, sheep: &str) -> Option<Model> {
    let models = by_sheep(config).remove(sheep)?;
    let (first, rest) = models.split_first()?;
    let mut stand_in = (*first).clone();
    stand_in.footprint = rest.iter().fold(first.footprint, |larger, model| {
        larger.larger(model.footprint)
    });
    let mut name = format!("sheep:{sheep}");
    while config.models.contains_key(&ModelName::from(name.as_str())) {
        name.insert_str(0, "sheep:");
    }
    stand_in.name = ModelName::from(name);
    Some(stand_in)
}

#[cfg(test)]
mod tests;
