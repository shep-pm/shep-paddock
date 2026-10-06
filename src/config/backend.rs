//! What serves a model, and when two models are one process.

use core::fmt;
use std::collections::BTreeMap;

use super::redacted;

/// What serves a model.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Backend {
    /// A sheep the dog starts and stops.
    Sheep {
        /// The sheep's name in the flock.
        sheep: String,
        /// What the sheep calls the model, when that differs from its name here.
        name: Option<String>,
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

// Env values and arguments, such as an --api-key, can carry credentials (IR-41), so only the
// env keys and the argument count are printed, and a url redacted.
impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sheep {
                sheep,
                name,
                args,
                env,
            } => f
                .debug_struct("Sheep")
                .field("sheep", sheep)
                .field("name", name)
                .field("arg_count", &args.as_ref().map(Vec::len))
                .field("env_keys", &env.keys().collect::<Vec<_>>())
                .finish(),
            Self::Ollama { url, name } => f
                .debug_struct("Ollama")
                .field("url", &redacted(url))
                .field("name", name)
                .finish(),
        }
    }
}

impl Backend {
    /// The sheep it runs on, if it is a sheep
    pub fn sheep(&self) -> Option<&str> {
        match self {
            Self::Sheep { sheep, .. } => Some(sheep),
            Self::Ollama { .. } => None,
        }
    }

    /// Whether a model on `self` and one on `other` would be served by one process
    ///
    /// Two sheep backends are when they name one sheep. Two ollama backends
    /// are when they name one server and one model, read as ollama reads it.
    pub fn same_process(&self, other: &Backend) -> bool {
        match (self, other) {
            (Self::Sheep { sheep: a, .. }, Self::Sheep { sheep: b, .. }) => a == b,
            (Self::Ollama { url: a, name: x }, Self::Ollama { url: b, name: y }) => {
                a == b && tagged(x) == tagged(y)
            }
            _ => false,
        }
    }
}

/// `name` with ollama's default tag when it has none
pub(crate) fn tagged(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') {
        name.to_owned()
    } else {
        format!("{name}:latest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama(url: &str, name: &str) -> Backend {
        Backend::Ollama {
            url: url.to_owned(),
            name: name.to_owned(),
        }
    }

    fn sheep(name: &str) -> Backend {
        Backend::Sheep {
            sheep: name.to_owned(),
            name: None,
            args: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn one_ollama_model_is_one_process_with_or_without_its_tag() {
        let here = "http://127.0.0.1:11434";
        assert!(ollama(here, "llama3").same_process(&ollama(here, "llama3:latest")));
        assert!(!ollama(here, "llama3").same_process(&ollama(here, "llama3:8b")));
        assert!(!ollama(here, "llama3").same_process(&ollama("http://other:11434", "llama3")));
    }

    #[test]
    fn one_sheep_is_one_process_and_never_an_ollama_one() {
        assert!(sheep("laya").same_process(&sheep("laya")));
        assert!(!sheep("laya").same_process(&sheep("iq3_s")));
        assert!(!sheep("laya").same_process(&ollama("http://127.0.0.1:11434", "laya")));
    }
}
