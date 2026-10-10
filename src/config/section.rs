//! The `[paddock]` section as it is written in `dogs.toml`, and the type
//! shep reads this dog's config schema off.
//!
//! Sizes and durations are read as strings and parsed in
//! [`Config::from_toml`](super::Config::from_toml) through shep's own
//! `FromStr`, so this crate carries no second copy of either grammar. The
//! schema attributes publish shep's grammar for each of them.
//!
//! `#[dog_config]` sits above the derives because it rewrites the fields it
//! marks and the derive has to see the rewrite.

use core::{fmt, num::NonZeroU32};
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
};
use shep_client::{
    dogs::dog_config,
    shep_core::values::{MemSize, UpDuration},
};

use super::Api;

/// What a model's `vram` may be: shep's size grammar, or the word `all`.
///
/// Exists only to publish that grammar in the schema, so a settings pane
/// accepts `all`, which `MemSize`'s own pattern would refuse.
struct VramSize;

impl JsonSchema for VramSize {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VramSize".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": r"^(\d+(G|M|K)?|all)$",
            "description": "A byte quantity: digits, optionally suffixed G, M or K (binary units). \
                            Or the word all, for a model that grows into whatever VRAM is free.",
        })
    }
}

/// The dog's settings. Every field has a default except the host's totals.
#[dog_config]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "paddock",
    description = "Settings for the shep-paddock dog: the host's GPU and RAM, the clients \
                   allowed to lease them, and the models served from them."
)]
pub(crate) struct Section {
    /// Address the dog's one endpoint listens on. Default 0.0.0.0:8700.
    pub(super) listen: Option<String>,
    /// How long a reclaimable model must be unused before a batch waiter evicts it.
    #[schemars(with = "Option<UpDuration>")]
    pub(super) grace: Option<String>,
    /// Default cap on how long a request waits for a lease.
    #[schemars(with = "Option<UpDuration>")]
    pub(super) max_wait: Option<String>,
    /// How long a connection-held lease survives a dog restart.
    #[schemars(with = "Option<UpDuration>")]
    pub(super) reconnect: Option<String>,
    /// What the host has to lease.
    pub(super) host: HostSection,
    /// The clients allowed to ask, one key each.
    #[serde(default)]
    pub(super) clients: Vec<ClientSection>,
    /// Named backends that models may share, by the name each is given here.
    #[serde(default)]
    pub(super) backends: BTreeMap<String, BackendSection>,
    /// The models, by the name clients ask for.
    #[serde(default)]
    pub(super) models: BTreeMap<String, ModelSection>,
}

/// The host's totals.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HostSection {
    /// Total VRAM, as nvidia-smi reports it.
    #[schemars(with = "MemSize")]
    pub(super) vram: String,
    /// Total RAM, as free reports it.
    #[schemars(with = "MemSize")]
    pub(super) ram: String,
}

/// One client and its key.
#[dog_config]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ClientSection {
    /// What the dog calls this client in its logs.
    pub(super) name: String,
    /// The bearer key this client presents.
    #[shep(secret)]
    pub(super) key: String,
}

/// The kinds of named backend.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum BackendKind {
    /// An ollama server the dog does not start.
    Ollama,
}

/// A named backend.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BackendSection {
    /// What sort of server this is.
    pub(super) kind: BackendKind,
    /// Where the server listens.
    pub(super) url: String,
}

/// A sheep the dog starts and stops for a model.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SheepBackend {
    /// The sheep to start.
    pub(super) sheep: String,
    /// Arguments parked on the sheep before it starts.
    pub(super) args: Option<Vec<String>>,
    /// Environment parked on the sheep before it starts.
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
}

/// A model's backend: the name of a `[backends]` entry, or a sheep inline.
#[derive(JsonSchema)]
#[serde(untagged)]
pub(super) enum BackendRef {
    /// A `[backends]` entry by name.
    Named(String),
    /// A sheep.
    Sheep(SheepBackend),
}

// Read by shape rather than untagged, so a typo inside the table reports
// the field instead of "did not match any variant".
impl<'de> Deserialize<'de> for BackendRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Shape;

        impl<'de> Visitor<'de> for Shape {
            type Value = BackendRef;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a backend name or an inline sheep table")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<BackendRef, E> {
                Ok(BackendRef::Named(name.to_owned()))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<BackendRef, A::Error> {
                SheepBackend::deserialize(MapAccessDeserializer::new(map)).map(BackendRef::Sheep)
            }
        }

        deserializer.deserialize_any(Shape)
    }
}

/// How the dog tells a started backend has loaded its model.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadySection {
    /// Path polled on the model's url.
    pub(super) path: String,
    /// A JSON field that must be present and truthy. Unset means any 200.
    pub(super) field: Option<String>,
}

/// One model.
#[dog_config]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelSection {
    /// A `[backends]` entry, or a sheep as `{ sheep, args, env }`.
    pub(super) backend: BackendRef,
    /// What the backend calls this model. A request's `model` is rewritten to it.
    pub(super) name: Option<String>,
    /// Where the backend serves this model. A sheep model needs it.
    pub(super) url: Option<String>,
    /// How to tell the model has loaded. Unset means ready once the sheep is online.
    pub(super) ready: Option<ReadySection>,
    /// The APIs the model speaks.
    #[serde(default)]
    pub(super) apis: Vec<Api>,
    /// A path prefix that routes to this model, stripped before forwarding.
    pub(super) prefix: Option<String>,
    /// The backend's own bearer key, sent in place of the client's.
    #[shep(secret)]
    pub(super) key: Option<String>,
    /// VRAM held while loaded: a size, or all. Unset means none.
    #[schemars(with = "Option<VramSize>")]
    pub(super) vram: Option<String>,
    /// RAM held while loaded. Unset means none.
    #[schemars(with = "Option<MemSize>")]
    pub(super) ram: Option<String>,
    /// Models that cannot be loaded beside this one.
    #[serde(default)]
    pub(super) excludes: Vec<String>,
    /// How long the model may sit unused before it is unloaded.
    #[schemars(with = "UpDuration")]
    pub(super) idle: String,
    /// How long a started backend has to become ready. Default 5m.
    #[schemars(with = "Option<UpDuration>")]
    pub(super) load_timeout: Option<String>,
    /// Ways the model can run, tried in this order when it loads. Replaces vram and ram.
    #[serde(default)]
    pub(super) placements: Vec<PlacementSection>,
    /// How many leases that are not reclaimable the backend serves at once. A lease past it
    /// waits its turn, and requests are never held back. Unset means no limit.
    pub(super) sequences: Option<NonZeroU32>,
}

/// One way a model can run, with its own footprint and the sheep fields it needs.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PlacementSection {
    /// What the status calls this placement.
    pub(super) name: String,
    /// VRAM held in this placement: a size, or all. Unset means none.
    #[schemars(with = "Option<VramSize>")]
    pub(super) vram: Option<String>,
    /// RAM held in this placement. Unset means none.
    #[schemars(with = "Option<MemSize>")]
    pub(super) ram: Option<String>,
    /// The sheep's script in this placement.
    pub(super) script: Option<String>,
    /// The sheep's arguments in this placement.
    pub(super) args: Option<Vec<String>>,
    /// Environment set on the sheep in this placement.
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
}
