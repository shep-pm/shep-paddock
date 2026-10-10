//! The `[paddock]` section, read and validated into what the book decides on.
//!
//! The shepherd hands the dog its section's text without the `[paddock]`
//! header, so [`Config::from_toml`] reads `listen` at the top level and
//! `[host]`, `[[clients]]`, `[backends.*]` and `[models.*]` below it.

use core::{fmt, num::NonZeroU32};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    time::Duration,
};

use reqwest::Url;
use schemars::JsonSchema;
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::footprint::{Footprint, Host};

mod backend;
mod check;
mod error;
mod model;
mod names;
mod placement;
pub(crate) mod section;
mod values;

#[cfg(test)]
mod tests;

pub(crate) use backend::{Backend, tagged};
use check::{
    check_clients, check_containers, check_exclusions, check_prefixes, check_shared_ollama,
    check_shared_sheep,
};
pub(crate) use error::ConfigError;
use model::{build_model, trim_slashes};
pub(crate) use names::{ClientName, ModelName, PlacementName};
pub(crate) use placement::Placement;
use section::{BackendKind, Section};
pub(crate) use values::redacted;
use values::{duration_or, parse_size};

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
    /// ollama's own API: `/api/chat`, `/api/generate`, `/api/embed` and `/api/embeddings`.
    Ollama,
}

/// How the dog tells a started backend has loaded its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ready {
    /// Path polled on the model's url.
    pub path: String,
    /// A JSON field that must be present and truthy. `None` means any 200.
    pub field: Option<String>,
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
    /// The url requests are forwarded to, parsed at load: the ollama server's, or a sheep model's own.
    pub base: Option<Url>,
    /// How to tell it has loaded.
    pub ready: Option<Ready>,
    /// The APIs it speaks.
    pub apis: Vec<Api>,
    /// A path prefix that routes to it.
    pub prefix: Option<String>,
    key: Option<String>,
    /// What it holds while loaded.
    ///
    /// With placements, the largest of each figure across them, which may match no one placement.
    /// It counts a run whose placement is unknown; a known one counts at [`Model::footprint_at`].
    pub footprint: Footprint,
    /// The ways it can run, in the order tried; empty for a model with one footprint.
    pub placements: Vec<Placement>,
    /// Models that cannot be loaded beside it, as written on this model.
    pub excludes: BTreeSet<ModelName>,
    /// How long it may sit unused before it is unloaded.
    pub idle: Duration,
    /// How long a started backend has to become ready.
    pub load_timeout: Duration,
    /// How many leases that are not reclaimable it serves at once, if it has a limit.
    pub sequences: Option<NonZeroU32>,
    /// The podman container its sheep starts, measured with it and stopped after it.
    pub container: Option<String>,
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
            .field("url", &self.url.as_deref().map(redacted))
            .field("ready", &self.ready)
            .field("apis", &self.apis)
            .field("prefix", &self.prefix)
            .field("footprint", &self.footprint)
            .field("placements", &self.placements)
            .field("excludes", &self.excludes)
            .field("idle", &self.idle)
            .field("load_timeout", &self.load_timeout)
            .field("sequences", &self.sequences)
            .field("container", &self.container)
            .finish_non_exhaustive()
    }
}

/// A client allowed to ask, and its key.
#[derive(Clone)]
pub(crate) struct Client {
    /// What the dog calls it.
    pub name: ClientName,
    /// Whether it may revoke any lease not held by a protected client.
    pub admin: bool,
    /// Whether other clients' revokes leave its leases alone.
    pub protected: bool,
    key: String,
}

impl Client {
    /// A client with `key` as given, which config loading would refuse if empty
    #[cfg(test)]
    pub fn with_key(name: ClientName, key: &str) -> Self {
        Self {
            name,
            admin: false,
            protected: false,
            key: key.to_owned(),
        }
    }

    /// Whether `presented` is this client's key, compared in constant time.
    pub fn key_matches(&self, presented: &[u8]) -> bool {
        self.key.as_bytes().ct_eq(presented).into()
    }
}

// The key is left out so that comparing clients never touches it.
impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.admin == other.admin && self.protected == other.protected
    }
}

impl Eq for Client {}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The dog's settings, validated.
#[derive(Clone, PartialEq, Eq)]
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
    /// Each ollama backend's name, by its url. Of two names on one url, the first sorted wins.
    pub ollamas: BTreeMap<String, String>,
}

// A url's userinfo, query or fragment can carry a credential, so the ollama urls are printed redacted.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ollamas: BTreeMap<_, _> = self
            .ollamas
            .iter()
            .map(|(url, name)| (redacted(url), name))
            .collect();
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("grace", &self.grace)
            .field("max_wait", &self.max_wait)
            .field("reconnect", &self.reconnect)
            .field("host", &self.host)
            .field("clients", &self.clients)
            .field("models", &self.models)
            .field("ollamas", &ollamas)
            .finish()
    }
}

impl Config {
    /// Parse the `[paddock]` section's body and validate it.
    ///
    /// # Errors
    /// - [`ConfigError::Toml`]: not valid TOML, or an unknown key.
    /// - [`ConfigError::Listen`], [`ConfigError::Size`],
    ///   [`ConfigError::Duration`]: a value outside the grammar it names.
    /// - [`ConfigError::EmptyKey`]: a client's key is empty.
    /// - [`ConfigError::DuplicateClientName`], [`ConfigError::DuplicateClientKey`]:
    ///   two clients share a name or a key.
    /// - [`ConfigError::UnknownBackend`]: a model names a backend that is not
    ///   defined.
    /// - [`ConfigError::MissingUrl`], [`ConfigError::MissingName`]: a sheep
    ///   model has no url, or an ollama model has no name.
    /// - [`ConfigError::BadUrl`]: a model's url, or its ollama backend's, does not parse, is not
    ///   http or https, or has no host.
    /// - [`ConfigError::NeverFits`]: a model is bigger than the host.
    /// - [`ConfigError::FootprintBesidePlacements`]: a model declares placements and its own
    ///   `vram` or `ram`.
    /// - [`ConfigError::PlacementsOnOllama`]: an ollama model declares placements.
    /// - [`ConfigError::DuplicatePlacement`]: a model declares two placements with one name.
    /// - [`ConfigError::PlacementKeysDiffer`]: a model's placements differ in whether they set
    ///   `script` or `args`, or in their `env` keys.
    /// - [`ConfigError::PlacementNeverFits`]: a placement is bigger than the host.
    /// - [`ConfigError::BadPrefix`]: a prefix does not start with `/` or ends with one.
    /// - [`ConfigError::DuplicatePrefix`]: two models share a prefix.
    /// - [`ConfigError::OverlappingPrefix`]: one prefix lies under another.
    /// - [`ConfigError::UnknownExclusion`]: `excludes` names no model.
    /// - [`ConfigError::SharedSheepMismatch`]: models on one sheep differ in
    ///   `env` keys or in whether they set `args` or a `script`, placements included, or their
    ///   container.
    /// - [`ConfigError::ContainerOnOllama`]: an ollama model names a container.
    /// - [`ConfigError::BadContainer`]: a container name is not one podman gives.
    /// - [`ConfigError::SharedContainer`]: models on two sheep name one container.
    /// - [`ConfigError::SharedOllamaModel`]: two models name one ollama model
    ///   on one server.
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
                admin: client.admin,
                protected: client.protected,
                key: client.key,
            });
        }

        let mut models = BTreeMap::new();
        for (name, model) in raw.models {
            let name = ModelName::from(name);
            let model = build_model(&name, model, &raw.backends, &host)?;
            models.insert(name, model);
        }

        let mut ollamas = BTreeMap::new();
        for (name, backend) in &raw.backends {
            match backend.kind {
                BackendKind::Ollama => {
                    ollamas
                        .entry(trim_slashes(&backend.url))
                        .or_insert_with(|| name.clone());
                }
            }
        }

        check_clients(&clients)?;
        check_prefixes(&models)?;
        check_exclusions(&models)?;
        check_shared_sheep(&models)?;
        check_containers(&models)?;
        check_shared_ollama(&models)?;

        Ok(Self {
            listen,
            grace: duration_or(raw.grace.as_deref(), "grace", DEFAULT_GRACE)?,
            max_wait: duration_or(raw.max_wait.as_deref(), "max_wait", DEFAULT_MAX_WAIT)?,
            reconnect: duration_or(raw.reconnect.as_deref(), "reconnect", DEFAULT_RECONNECT)?,
            host,
            clients,
            models,
            ollamas,
        })
    }

    /// Whether two models may not be loaded together
    ///
    /// Either may name the other, or both run on one sheep, which runs one process.
    pub fn excluded(&self, a: &ModelName, b: &ModelName) -> bool {
        let names = |from: &ModelName, other: &ModelName| {
            self.models
                .get(from)
                .is_some_and(|model| model.excludes.contains(other))
        };
        let sheep = |model: &ModelName| self.models.get(model).and_then(|m| m.backend.sheep());
        let shared = a != b && sheep(a).is_some() && sheep(a) == sheep(b);
        shared || names(a, b) || names(b, a)
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
