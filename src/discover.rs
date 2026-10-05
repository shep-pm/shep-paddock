//! Finding what is loaded when the dog starts, before it listens.
//!
//! A sheep that a configured model runs on, and that the flock shows running
//! or waiting to restart, serves the model the saved state names for it. That
//! model must pass its ready check within a few tries, unless a saved lease
//! names it. Otherwise the sheep counts as unknown at the largest footprint of
//! the models on it. An ollama model is loaded when `/api/ps` lists its name
//! and its ready check passes, or a saved lease names it. Anything else
//! `/api/ps` lists counts as unknown at the figures it reports. A name with no
//! tag matches `<name>:latest`.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use shep_client::shep_core::status::ProcStatus;
use tokio::time::sleep;

use crate::{
    backend::{Backends, OllamaLoaded},
    config::{Backend, Config, Model, ModelName, tagged},
    footprint::Footprint,
    saved::Saved,
    shepherd::Shepherd,
};

// A backend that has just come up may not answer ready at once. Three tries a
// second apart add at most two seconds of pauses to a start.
const READY_TRIES: u32 = 3;
const READY_PAUSE: Duration = Duration::from_secs(1);

/// What discovery found holding memory
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Discovered {
    /// Each model found loaded, unknown ones under their [`stand_in`] names.
    pub loaded: Vec<(ModelName, Footprint)>,
    /// The running sheep whose model is not known, by sheep name.
    pub unknown: Vec<String>,
    /// The model each unknown counts as, sheep and ollama alike, for the engine to unload by.
    pub stand_ins: Vec<Model>,
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
    let leased: BTreeSet<&ModelName> = saved.leases.iter().map(|lease| &lease.model).collect();
    let running: BTreeSet<String> = match backends.shepherd().list_flock().await {
        Ok(flock) => flock
            .into_iter()
            // A sheep waiting to restart will run again, and nothing would map it to a model then.
            .filter(|row| {
                matches!(
                    row.status,
                    ProcStatus::Starting | ProcStatus::Online | ProcStatus::WaitingRestart
                )
            })
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
        // A lease must keep its model across a restart. If the sheep is dead,
        // the engine's first flock listing finds it.
        match named {
            Some(model) if leased.contains(&model.name) || ready_soon(backends, model).await => {
                found.loaded.push((model.name.clone(), model.footprint));
            }
            _ => found.unknown(config, sheep),
        }
    }
    for (url, models) in by_ollama(config) {
        let key = models.iter().find_map(|model| model.key());
        let listed = match backends.ollama_loaded(url, key).await {
            Ok(listed) => listed,
            Err(err) => {
                eprintln!("paddock: asking ollama what it has loaded failed: {err}");
                continue;
            }
        };
        let mut restored = BTreeSet::new();
        for model in &models {
            let Backend::Ollama { name, .. } = &model.backend else {
                continue;
            };
            let name = tagged(name);
            let is_listed = listed.iter().any(|loaded| tagged(&loaded.name) == name);
            let held = leased.contains(&model.name);
            if is_listed && (held || ready_soon(backends, model).await) {
                found.loaded.push((model.name.clone(), model.footprint));
                restored.insert(name);
            }
        }
        let Some(like) = models.first() else {
            continue;
        };
        for loaded in listed {
            if !restored.contains(&tagged(&loaded.name)) {
                found.stand_in_for(ollama_stand_in(config, like, url, loaded));
            }
        }
    }
    found
}

/// Whether `model`'s ready check passes within [`READY_TRIES`] tries
async fn ready_soon<S: Shepherd>(backends: &Backends<S>, model: &Model) -> bool {
    for tried in 1..=READY_TRIES {
        if backends.ready_now(model).await {
            return true;
        }
        if tried < READY_TRIES {
            sleep(READY_PAUSE).await;
        }
    }
    false
}

impl Discovered {
    /// Counts `sheep` as running a model nobody named
    fn unknown(&mut self, config: &Config, sheep: &str) {
        if let Some(model) = stand_in(config, sheep) {
            self.unknown.push(sheep.to_owned());
            self.stand_in_for(model);
        }
    }

    fn stand_in_for(&mut self, model: Model) {
        self.loaded.push((model.name.clone(), model.footprint));
        self.stand_ins.push(model);
    }
}

/// `prefix` and `name`, with `prefix` repeated until no configured model has it
fn unclaimed(config: &Config, prefix: &str, name: &str) -> ModelName {
    let mut candidate = format!("{prefix}{name}");
    while config
        .models
        .contains_key(&ModelName::from(candidate.as_str()))
    {
        candidate.insert_str(0, prefix);
    }
    ModelName::from(candidate)
}

/// The model an unknown ollama model counts as: at the figures ollama reports,
/// unloaded through the same ollama as `like`
fn ollama_stand_in(config: &Config, like: &Model, url: &str, loaded: OllamaLoaded) -> Model {
    let mut stand_in = like.clone();
    stand_in.name = unclaimed(config, "ollama:", &loaded.name);
    stand_in.backend = Backend::Ollama {
        url: url.to_owned(),
        name: loaded.name,
    };
    stand_in.footprint = loaded.footprint;
    stand_in.ready = None;
    stand_in.prefix = None;
    stand_in.excludes.clear();
    stand_in
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
    stand_in.name = unclaimed(config, "sheep:", sheep);
    Some(stand_in)
}

#[cfg(test)]
mod tests;
