//! The checks across a section's models that no single model can make.

use std::collections::{BTreeMap, BTreeSet};

use super::{Backend, Client, ClientName, ConfigError, Model, ModelName, redacted, tagged};

pub(super) fn check_clients(clients: &[Client]) -> Result<(), ConfigError> {
    let mut names = BTreeSet::new();
    let mut keys: BTreeMap<&str, &ClientName> = BTreeMap::new();
    for client in clients {
        if !names.insert(&client.name) {
            return Err(ConfigError::DuplicateClientName {
                name: client.name.clone(),
            });
        }
        if let Some(first) = keys.insert(&client.key, &client.name) {
            return Err(ConfigError::DuplicateClientKey {
                first: first.clone(),
                second: client.name.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn check_prefixes(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    let mut seen: BTreeMap<&str, &ModelName> = BTreeMap::new();
    for model in models.values() {
        let Some(prefix) = model.prefix.as_deref() else {
            continue;
        };
        if !prefix.starts_with('/') || prefix.ends_with('/') {
            return Err(ConfigError::BadPrefix {
                model: model.name.clone(),
                prefix: prefix.to_owned(),
            });
        }
        if let Some(first) = seen.insert(prefix, &model.name) {
            return Err(ConfigError::DuplicatePrefix {
                prefix: prefix.to_owned(),
                first: first.clone(),
                second: model.name.clone(),
            });
        }
    }
    let prefixed: Vec<(&str, &ModelName)> = seen.into_iter().collect();
    for (at, (outer, outer_model)) in prefixed.iter().enumerate() {
        let inner = prefixed[at + 1..].iter().find(|(longer, _)| {
            longer
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
        });
        if let Some((inner, inner_model)) = inner {
            return Err(ConfigError::OverlappingPrefix {
                outer: (*outer).to_owned(),
                outer_model: (*outer_model).clone(),
                inner: (*inner).to_owned(),
                inner_model: (*inner_model).clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn check_exclusions(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    for model in models.values() {
        if let Some(unknown) = model
            .excludes
            .iter()
            .find(|name| !models.contains_key(*name))
        {
            return Err(ConfigError::UnknownExclusion {
                model: model.name.clone(),
                excluded: unknown.to_string(),
            });
        }
    }
    Ok(())
}

pub(super) fn check_shared_sheep(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    let mut first_on: BTreeMap<&str, &Model> = BTreeMap::new();
    for model in models.values() {
        let Backend::Sheep {
            sheep, args, env, ..
        } = &model.backend
        else {
            continue;
        };
        let Some(first) = first_on.get(sheep.as_str()) else {
            first_on.insert(sheep, model);
            continue;
        };
        let Backend::Sheep {
            args: first_args,
            env: first_env,
            ..
        } = &first.backend
        else {
            continue;
        };
        let what = if !env.keys().eq(first_env.keys()) {
            "env keys"
        } else if args.is_some() != first_args.is_some() {
            "args"
        } else {
            continue;
        };
        return Err(ConfigError::SharedSheepMismatch {
            sheep: sheep.clone(),
            first: first.name.clone(),
            second: model.name.clone(),
            what,
        });
    }
    Ok(())
}

pub(super) fn check_shared_ollama(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    let ollama: Vec<_> = models
        .values()
        .filter_map(|model| match &model.backend {
            Backend::Ollama { url, name } => Some((model, url, name)),
            Backend::Sheep { .. } => None,
        })
        .collect();
    for (at, (first, url, name)) in ollama.iter().enumerate() {
        if let Some((second, ..)) = ollama[at + 1..]
            .iter()
            .find(|(other, ..)| first.backend.same_process(&other.backend))
        {
            return Err(ConfigError::SharedOllamaModel {
                url: redacted(url),
                name: tagged(name),
                first: first.name.clone(),
                second: second.name.clone(),
            });
        }
    }
    Ok(())
}
