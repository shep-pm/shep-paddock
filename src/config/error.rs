//! What `[paddock]` can be refused for.

use core::fmt;
use std::net::AddrParseError;

use shep_client::shep_core::values::{ParseMemSizeError, ParseUpDurationError};

use super::{ClientName, ModelName};

/// What `[paddock]` was refused for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigError {
    /// The text is not valid TOML, or carries a key this dog does not know.
    Toml(String),
    /// `listen` is not a socket address.
    Listen {
        /// The value as written.
        value: String,
        /// The underlying parse failure.
        source: AddrParseError,
    },
    /// A size is not spelled the way shep spells one.
    Size {
        /// The field, as a path into the section.
        field: String,
        /// The value as written.
        value: String,
        /// The underlying parse failure.
        source: ParseMemSizeError,
    },
    /// A duration is not spelled the way shep spells one.
    Duration {
        /// The field, as a path into the section.
        field: String,
        /// The value as written.
        value: String,
        /// The underlying parse failure.
        source: ParseUpDurationError,
    },
    /// A client's key is empty, which would let any request through as it.
    EmptyKey {
        /// The client.
        client: ClientName,
    },
    /// Two clients share one `name`, so a log line could not say which asked.
    DuplicateClientName {
        /// The shared name.
        name: ClientName,
    },
    /// Two clients share one `key`, so the first would answer for both.
    DuplicateClientKey {
        /// The first client, in file order.
        first: ClientName,
        /// The second client.
        second: ClientName,
    },
    /// A model names a backend that `[backends]` does not define.
    UnknownBackend {
        /// The model.
        model: ModelName,
        /// The backend name it used.
        backend: String,
    },
    /// A sheep model sets no `url`, so the dog has nowhere to forward to.
    MissingUrl {
        /// The model.
        model: ModelName,
    },
    /// An ollama model sets no `name`, so the dog cannot say what to load.
    MissingName {
        /// The model.
        model: ModelName,
    },
    /// A model's footprint exceeds the host even with nothing else loaded.
    NeverFits {
        /// The model.
        model: ModelName,
    },
    /// Two models share one `prefix`.
    DuplicatePrefix {
        /// The shared prefix.
        prefix: String,
        /// The first model, in name order.
        first: ModelName,
        /// The second model.
        second: ModelName,
    },
    /// One model's `prefix` is a path-segment prefix of another's, so a path
    /// under the longer one would also match the shorter.
    OverlappingPrefix {
        /// The shorter prefix.
        outer: String,
        /// The model that has it.
        outer_model: ModelName,
        /// The longer prefix.
        inner: String,
        /// The model that has it.
        inner_model: ModelName,
    },
    /// A model's `prefix` does not start with `/`, or ends with one, so it
    /// does not end on a path segment.
    BadPrefix {
        /// The model.
        model: ModelName,
        /// The prefix as written.
        prefix: String,
    },
    /// A model's `excludes` names a model that is not configured.
    UnknownExclusion {
        /// The model that carries the `excludes`.
        model: ModelName,
        /// The name it excludes.
        excluded: String,
    },
    /// Two models on one sheep disagree about `env` keys or about whether
    /// `args` is set, so a value one sets would outlive it into the next.
    SharedSheepMismatch {
        /// The shared sheep.
        sheep: String,
        /// The first model, in name order.
        first: ModelName,
        /// The model that differs from it.
        second: ModelName,
        /// What differs: `env keys` or `args`.
        what: &'static str,
    },
    /// Two models name one ollama model on one server, so its memory would
    /// be counted twice.
    SharedOllamaModel {
        /// The server's url.
        url: String,
        /// The ollama model, with ollama's default tag when it has none.
        name: String,
        /// The first model, in name order.
        first: ModelName,
        /// The second model.
        second: ModelName,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(message) => write!(f, "invalid TOML: {message}"),
            Self::Listen { value, source } => {
                write!(f, "listen = \"{value}\" is not a socket address: {source}")
            }
            Self::Size {
                field,
                value,
                source,
            } => write!(
                f,
                "{field} = \"{value}\" is not a size shep accepts: {source}"
            ),
            Self::Duration {
                field,
                value,
                source,
            } => write!(
                f,
                "{field} = \"{value}\" is not a duration shep accepts: {source}"
            ),
            Self::EmptyKey { client } => write!(f, "client \"{client}\" has an empty key"),
            Self::DuplicateClientName { name } => {
                write!(f, "two clients are named \"{name}\"")
            }
            Self::DuplicateClientKey { first, second } => {
                write!(f, "clients \"{first}\" and \"{second}\" share one key")
            }
            Self::UnknownBackend { model, backend } => write!(
                f,
                "model \"{model}\" names backend \"{backend}\", which [backends] does not define"
            ),
            Self::MissingUrl { model } => write!(f, "sheep model \"{model}\" needs a url"),
            Self::MissingName { model } => write!(f, "ollama model \"{model}\" needs a name"),
            Self::NeverFits { model } => {
                write!(f, "model \"{model}\" cannot fit the host even when alone")
            }
            Self::DuplicatePrefix {
                prefix,
                first,
                second,
            } => write!(
                f,
                "models \"{first}\" and \"{second}\" share the prefix \"{prefix}\""
            ),
            Self::OverlappingPrefix {
                outer,
                outer_model,
                inner,
                inner_model,
            } => write!(
                f,
                "model \"{outer_model}\" has prefix \"{outer}\", which contains \"{inner}\" of model \"{inner_model}\""
            ),
            Self::BadPrefix { model, prefix } => write!(
                f,
                "model \"{model}\" has prefix \"{prefix}\", which must start with / and not end with one"
            ),
            Self::UnknownExclusion { model, excluded } => write!(
                f,
                "model \"{model}\" excludes \"{excluded}\", which is not a model"
            ),
            Self::SharedSheepMismatch {
                sheep,
                first,
                second,
                what,
            } => write!(
                f,
                "models \"{first}\" and \"{second}\" share sheep \"{sheep}\" but differ in {what}"
            ),
            Self::SharedOllamaModel {
                url,
                name,
                first,
                second,
            } => write!(
                f,
                "models \"{first}\" and \"{second}\" both name ollama model \"{name}\" at {url}"
            ),
        }
    }
}

impl core::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Listen { source, .. } => Some(source),
            Self::Size { source, .. } => Some(source),
            Self::Duration { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ConfigError {
    /// A TOML failure, located but never quoted.
    ///
    /// toml's own message carries the offending source line, and serde's
    /// "invalid type" messages carry the value, so a mistyped `key` would
    /// print the credential (IR-41). Only the messages that name a key and
    /// no value are kept.
    pub(super) fn from_toml_error(err: &toml::de::Error, text: &str) -> Self {
        let message = err.message();
        let safe = ["unknown field", "missing field", "duplicate key"]
            .iter()
            .any(|prefix| message.starts_with(prefix));
        let what = if safe {
            message
        } else {
            "a value of the wrong type or form"
        };
        // A span that does not fall on a character boundary of `text` goes unlocated.
        let before = err
            .span()
            .and_then(|span| text.get(..span.start.min(text.len())));
        let location = before.map(|before| {
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
            format!(" at line {line}, column {column}")
        });
        Self::Toml(format!("{what}{}", location.unwrap_or_default()))
    }
}
