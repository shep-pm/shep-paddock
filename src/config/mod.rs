//! The `[paddock]` section, read and validated into what the book decides on.
//!
//! The shepherd hands the dog its section's text without the `[paddock]`
//! header, so [`Config::from_toml`] reads `listen` at the top level and
//! `[host]`, `[[clients]]`, `[backends.*]` and `[models.*]` below it.

use core::fmt;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    time::Duration,
};

use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::shep_core::values::{MemSize, UpDuration};
use subtle::ConstantTimeEq;

use crate::footprint::{Footprint, Host, Vram};

mod error;
mod names;
pub(crate) mod section;

#[cfg(test)]
mod tests;

pub(crate) use error::ConfigError;
pub(crate) use names::{ClientName, ModelName};
use section::{BackendKind, BackendRef, ModelSection, Section};

const DEFAULT_LISTEN: &str = "0.0.0.0:8700";
const DEFAULT_GRACE: Duration = Duration::from_secs(120);
const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(120);
const DEFAULT_RECONNECT: Duration = Duration::from_secs(60);
const DEFAULT_LOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// An API a model speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub(crate) enum Api {
    /// The OpenAI-compatible API.
    OpenAi,
    /// The Anthropic messages API.
    Anthropic,
}

/// How the dog tells a started backend has loaded its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ready {
    /// Path polled on the model's url.
    pub path: String,
    /// A JSON field that must be present and truthy. `None` means any 200.
    pub field: Option<String>,
}

/// What serves a model.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Backend {
    /// A sheep the dog starts and stops.
    Sheep {
        /// The sheep's name in the flock.
        sheep: String,
        /// Arguments parked on the sheep before it starts, when set.
        args: Option<Vec<String>>,
        /// Environment parked on the sheep before it starts.
        env: BTreeMap<String, String>,
    },
    /// An ollama server the dog does not start.
    Ollama {
        /// Where the server listens.
        url: String,
        /// What ollama calls the model.
        name: String,
    },
}

// Env values can carry credentials (IR-41), so only the keys are printed.
impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sheep { sheep, args, env } => f
                .debug_struct("Sheep")
                .field("sheep", sheep)
                .field("args", args)
                .field("env_keys", &env.keys().collect::<Vec<_>>())
                .finish(),
            Self::Ollama { url, name } => f
                .debug_struct("Ollama")
                .field("url", url)
                .field("name", name)
                .finish(),
        }
    }
}

/// One model, as the book and the router see it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Model {
    /// The name clients ask for.
    pub name: ModelName,
    /// What serves it.
    pub backend: Backend,
    /// Where the backend serves it.
    pub url: Option<String>,
    /// How to tell it has loaded.
    pub ready: Option<Ready>,
    /// The APIs it speaks.
    pub apis: Vec<Api>,
    /// A path prefix that routes to it.
    pub prefix: Option<String>,
    key: Option<String>,
    /// What it holds while loaded.
    pub footprint: Footprint,
    /// Models that cannot be loaded beside it, as written on this model.
    pub excludes: BTreeSet<ModelName>,
    /// How long it may sit unused before it is unloaded.
    pub idle: Duration,
    /// How long a started backend has to become ready.
    pub load_timeout: Duration,
}

impl Model {
    /// The backend's own bearer key, sent in place of the client's.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model")
            .field("name", &self.name)
            .field("backend", &self.backend)
            .field("url", &self.url)
            .field("ready", &self.ready)
            .field("apis", &self.apis)
            .field("prefix", &self.prefix)
            .field("footprint", &self.footprint)
            .field("excludes", &self.excludes)
            .field("idle", &self.idle)
            .field("load_timeout", &self.load_timeout)
            .finish_non_exhaustive()
    }
}

/// A client allowed to ask, and its key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Client {
    /// What the dog calls it.
    pub name: ClientName,
    key: String,
}

impl Client {
    /// Whether `presented` is this client's key, compared in constant time.
    pub fn key_matches(&self, presented: &[u8]) -> bool {
        self.key.as_bytes().ct_eq(presented).into()
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The dog's settings, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// Where the endpoint listens.
    pub listen: SocketAddr,
    /// How long a reclaimable model must be unused before a batch waiter evicts it.
    pub grace: Duration,
    /// Default cap on how long a request waits.
    pub max_wait: Duration,
    /// How long a connection-held lease survives a dog restart.
    pub reconnect: Duration,
    /// What the host has to lease.
    pub host: Host,
    /// The clients allowed to ask.
    pub clients: Vec<Client>,
    /// The models, by name.
    pub models: BTreeMap<ModelName, Model>,
}

fn parse_size(value: &str, field: &str) -> Result<MemSize, ConfigError> {
    value.parse().map_err(|source| ConfigError::Size {
        field: field.to_owned(),
        value: value.to_owned(),
        source,
    })
}

fn parse_duration(value: &str, field: &str) -> Result<Duration, ConfigError> {
    value
        .parse::<UpDuration>()
        .map(UpDuration::as_duration)
        .map_err(|source| ConfigError::Duration {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

fn duration_or(
    value: Option<&str>,
    field: &str,
    default: Duration,
) -> Result<Duration, ConfigError> {
    value.map_or(Ok(default), |value| parse_duration(value, field))
}

impl Config {
    /// Parse the `[paddock]` section's body and validate it.
    ///
    /// # Errors
    /// - [`ConfigError::Toml`]: not valid TOML, or an unknown key.
    /// - [`ConfigError::Listen`], [`ConfigError::Size`],
    ///   [`ConfigError::Duration`]: a value outside the grammar it names.
    /// - [`ConfigError::EmptyKey`]: a client's key is empty.
    /// - [`ConfigError::UnknownBackend`]: a model names a backend that is not
    ///   defined.
    /// - [`ConfigError::MissingUrl`], [`ConfigError::MissingName`]: a sheep
    ///   model has no url, or an ollama model has no name.
    /// - [`ConfigError::NeverFits`]: a model is bigger than the host.
    /// - [`ConfigError::DuplicatePrefix`]: two models share a prefix.
    /// - [`ConfigError::UnknownExclusion`]: `excludes` names no model.
    /// - [`ConfigError::SharedSheepMismatch`]: models on one sheep differ in
    ///   `env` keys or in whether they set `args`.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let raw: Section =
            toml::from_str(text).map_err(|err| ConfigError::from_toml_error(&err, text))?;

        let listen_text = raw.listen.as_deref().unwrap_or(DEFAULT_LISTEN);
        let listen = listen_text.parse().map_err(|source| ConfigError::Listen {
            value: listen_text.to_owned(),
            source,
        })?;
        let host = Host {
            vram: parse_size(&raw.host.vram, "host.vram")?.bytes(),
            ram: parse_size(&raw.host.ram, "host.ram")?.bytes(),
        };

        let mut clients = Vec::with_capacity(raw.clients.len());
        for client in raw.clients {
            if client.key.is_empty() {
                return Err(ConfigError::EmptyKey {
                    client: client.name.into(),
                });
            }
            clients.push(Client {
                name: client.name.into(),
                key: client.key,
            });
        }

        let mut models = BTreeMap::new();
        for (name, model) in raw.models {
            let name = ModelName::from(name);
            let model = build_model(&name, model, &raw.backends, &host)?;
            models.insert(name, model);
        }

        check_prefixes(&models)?;
        check_exclusions(&models)?;
        check_shared_sheep(&models)?;

        Ok(Self {
            listen,
            grace: duration_or(raw.grace.as_deref(), "grace", DEFAULT_GRACE)?,
            max_wait: duration_or(raw.max_wait.as_deref(), "max_wait", DEFAULT_MAX_WAIT)?,
            reconnect: duration_or(raw.reconnect.as_deref(), "reconnect", DEFAULT_RECONNECT)?,
            host,
            clients,
            models,
        })
    }

    /// Whether two models may not be loaded together. Either may name the other.
    pub fn excluded(&self, a: &ModelName, b: &ModelName) -> bool {
        let names = |from: &ModelName, other: &ModelName| {
            self.models
                .get(from)
                .is_some_and(|model| model.excludes.contains(other))
        };
        names(a, b) || names(b, a)
    }

    /// The client whose key is `presented`.
    ///
    /// Every client is compared, so the time taken does not say which one
    /// matched or how many came first.
    pub fn client_for_key(&self, presented: &[u8]) -> Option<&Client> {
        self.clients.iter().fold(None, |found, client| {
            let matched = client.key_matches(presented);
            found.or(matched.then_some(client))
        })
    }

    /// The model whose prefix starts `path` on a segment boundary.
    pub fn model_for_prefix(&self, path: &str) -> Option<&Model> {
        self.models.values().find(|model| {
            model.prefix.as_deref().is_some_and(|prefix| {
                path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
    }
}

fn build_model(
    name: &ModelName,
    raw: ModelSection,
    backends: &BTreeMap<String, section::BackendSection>,
    host: &Host,
) -> Result<Model, ConfigError> {
    let field = |leaf: &str| format!("models.{name}.{leaf}");
    let (backend, url) = match raw.backend {
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
                    (
                        Backend::Ollama {
                            url: named.url.clone(),
                            name: model_name,
                        },
                        raw.url.or_else(|| Some(named.url.clone())),
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
            (
                Backend::Sheep {
                    sheep: sheep.sheep,
                    args: sheep.args,
                    env: sheep.env,
                },
                raw.url,
            )
        }
    };

    let vram = match raw.vram.as_deref() {
        None => Vram::None,
        Some("all") => Vram::All,
        Some(size) => Vram::Bytes(parse_size(size, &field("vram"))?.bytes()),
    };
    let ram = raw
        .ram
        .as_deref()
        .map(|size| parse_size(size, &field("ram")))
        .transpose()?
        .map_or(0, MemSize::bytes);
    let footprint = Footprint { vram, ram };
    if !host.ever_fits(&footprint) {
        return Err(ConfigError::NeverFits {
            model: name.clone(),
        });
    }

    Ok(Model {
        name: name.clone(),
        backend,
        url,
        ready: raw.ready.map(|ready| Ready {
            path: ready.path,
            field: ready.field,
        }),
        apis: raw.apis,
        prefix: raw.prefix,
        key: raw.key,
        footprint,
        excludes: raw.excludes.into_iter().map(ModelName::from).collect(),
        idle: parse_duration(&raw.idle, &field("idle"))?,
        load_timeout: duration_or(
            raw.load_timeout.as_deref(),
            &field("load_timeout"),
            DEFAULT_LOAD_TIMEOUT,
        )?,
    })
}

fn check_prefixes(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    let mut seen: BTreeMap<&str, &ModelName> = BTreeMap::new();
    for model in models.values() {
        let Some(prefix) = model.prefix.as_deref() else {
            continue;
        };
        if let Some(first) = seen.insert(prefix, &model.name) {
            return Err(ConfigError::DuplicatePrefix {
                prefix: prefix.to_owned(),
                first: first.clone(),
                second: model.name.clone(),
            });
        }
    }
    Ok(())
}

fn check_exclusions(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
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

fn check_shared_sheep(models: &BTreeMap<ModelName, Model>) -> Result<(), ConfigError> {
    let mut first_on: BTreeMap<&str, &Model> = BTreeMap::new();
    for model in models.values() {
        let Backend::Sheep { sheep, args, env } = &model.backend else {
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
