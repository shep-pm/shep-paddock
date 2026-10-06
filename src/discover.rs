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

use futures_util::future::{join, join_all};
use shep_client::shep_core::status::ProcStatus;
use tokio::time::sleep;

use crate::{
    backend::{Backends, LoadError, OllamaLoaded},
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
    /// The model each unknown counts as, sheep and ollama alike, for the engine to unload by.
    pub stand_ins: Vec<Model>,
    /// Each model whose backend could not be asked, and the error the status reports for it.
    pub unasked: Vec<(ModelName, String)>,
}

/// Finds every model loaded on the host, asking each backend once
///
/// Every backend is asked at once, so a start waits for the slowest, not the
/// sum. A shepherd or an ollama that does not answer is logged, and nothing on
/// it counts. Each model on a silent ollama goes in [`Discovered::unasked`].
pub(crate) async fn discover<S: Shepherd>(
    config: &Config,
    backends: &Backends<S>,
    saved: &Saved,
) -> Discovered {
    let leased: BTreeSet<&ModelName> = saved.leases.iter().map(|lease| &lease.model).collect();
    let running = running_sheep(backends).await;
    let sheep: Vec<_> = by_sheep(config)
        .into_iter()
        .filter(|(sheep, _)| running.contains(*sheep))
        .collect();
    let ollamas: Vec<_> = by_ollama(config).into_iter().collect();
    let (serving, answered) = join(
        join_all(
            sheep
                .iter()
                .map(|(sheep, models)| saved_model_up(backends, saved, &leased, sheep, models)),
        ),
        join_all(
            ollamas
                .iter()
                .map(|(url, models)| ask_ollama(backends, &leased, url, models)),
        ),
    )
    .await;
    let mut found = Discovered::default();
    for ((sheep, _), serving) in sheep.iter().zip(serving) {
        match serving {
            Some(model) => found.loaded.push((model.name.clone(), model.footprint)),
            None => {
                if let Some(model) = stand_in(config, sheep) {
                    found.stand_in_for(model);
                }
            }
        }
    }
    for ((url, models), answered) in ollamas.iter().zip(answered) {
        found.ollama(config, url, models, answered);
    }
    found
}

/// The sheep the flock shows running or waiting to restart
async fn running_sheep<S: Shepherd>(backends: &Backends<S>) -> BTreeSet<String> {
    match backends.shepherd().list_flock().await {
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
    }
}

/// The model the saved state names for `sheep`, when a lease names it or it is ready
async fn saved_model_up<'a, S: Shepherd>(
    backends: &Backends<S>,
    saved: &Saved,
    leased: &BTreeSet<&ModelName>,
    sheep: &str,
    models: &[&'a Model],
) -> Option<&'a Model> {
    let named = saved.sheep.get(sheep)?;
    let model = models.iter().copied().find(|model| model.name == *named)?;
    // A lease must keep its model across a restart. If the sheep is dead,
    // the engine's first flock listing finds it.
    let up = leased.contains(&model.name) || ready_soon(backends, model).await;
    up.then_some(model)
}

/// What the ollama at `url` lists, and the configured models on it that count as loaded
///
/// A configured model counts when `/api/ps` lists it and a lease names it or
/// it is ready. Its ready checks run at once.
async fn ask_ollama<'a, S: Shepherd>(
    backends: &Backends<S>,
    leased: &BTreeSet<&ModelName>,
    url: &str,
    models: &[&'a Model],
) -> Result<(Vec<OllamaLoaded>, Vec<&'a Model>), LoadError> {
    let key = keyed(models).and_then(Model::key);
    let listed = backends.ollama_loaded(url, key).await?;
    let restored = join_all(models.iter().map(|&model| {
        let listed = &listed;
        async move {
            let Backend::Ollama { name, .. } = &model.backend else {
                return None;
            };
            let name = tagged(name);
            let is_listed = listed.iter().any(|loaded| tagged(&loaded.name) == name);
            let up =
                is_listed && (leased.contains(&model.name) || ready_soon(backends, model).await);
            up.then_some(model)
        }
    }))
    .await;
    Ok((listed, restored.into_iter().flatten().collect()))
}

/// The model on one ollama whose key reads `/api/ps`, or the first if none has a key
///
/// Stand-ins are cloned from it, so the key that listed a model unloads it.
fn keyed<'a>(models: &[&'a Model]) -> Option<&'a Model> {
    models
        .iter()
        .copied()
        .find(|model| model.key().is_some())
        .or_else(|| models.first().copied())
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
    /// Counts what the ollama at `url` answered, or records each of its models unasked
    fn ollama(
        &mut self,
        config: &Config,
        url: &str,
        models: &[&Model],
        answered: Result<(Vec<OllamaLoaded>, Vec<&Model>), LoadError>,
    ) {
        let (listed, restored) = match answered {
            Ok(answered) => answered,
            Err(err) => {
                eprintln!("paddock: asking ollama what it has loaded failed: {err}");
                let error = format!(
                    "backend {} did not answer at start: {err}",
                    backend(config, url)
                );
                for model in models {
                    self.unasked.push((model.name.clone(), error.clone()));
                }
                return;
            }
        };
        let mut names = BTreeSet::new();
        for model in restored {
            self.loaded.push((model.name.clone(), model.footprint));
            if let Backend::Ollama { name, .. } = &model.backend {
                names.insert(tagged(name));
            }
        }
        let Some(like) = keyed(models) else {
            return;
        };
        for loaded in listed {
            if !names.contains(&tagged(&loaded.name)) {
                let stand_in = ollama_stand_in(config, &self.loaded, like, url, loaded);
                self.stand_in_for(stand_in);
            }
        }
    }

    fn stand_in_for(&mut self, model: Model) {
        self.loaded.push((model.name.clone(), model.footprint));
        self.stand_ins.push(model);
    }
}

/// `prefix` and `name`, with `prefix` repeated until neither a configured
/// model nor one in `taken` has it
fn unclaimed(
    config: &Config,
    taken: &[(ModelName, Footprint)],
    prefix: &str,
    name: &str,
) -> ModelName {
    let mut candidate = ModelName::from(format!("{prefix}{name}"));
    while config.models.contains_key(&candidate) || taken.iter().any(|(t, _)| *t == candidate) {
        candidate = ModelName::from(format!("{prefix}{candidate}"));
    }
    candidate
}

/// The model an unknown ollama model counts as: named for its backend, at the
/// figures ollama reports, unloaded through the same ollama as `like`
fn ollama_stand_in(
    config: &Config,
    taken: &[(ModelName, Footprint)],
    like: &Model,
    url: &str,
    loaded: OllamaLoaded,
) -> Model {
    let mut stand_in = like.clone();
    let prefix = format!("{}:", backend(config, url));
    stand_in.name = unclaimed(config, taken, &prefix, &loaded.name);
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

/// The name the config gives the ollama backend at `url`
fn backend<'a>(config: &'a Config, url: &str) -> &'a str {
    config.ollamas.get(url).map_or("ollama", String::as_str)
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
    stand_in.name = unclaimed(config, &[], "sheep:", sheep);
    Some(stand_in)
}

#[cfg(test)]
mod tests;
