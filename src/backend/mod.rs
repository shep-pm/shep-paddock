//! Loading and unloading a model on the backend that serves it.
//!
//! A sheep model is loaded by parking its env and args on the sheep, restarting it and waiting
//! for its ready check, or for the sheep to come online when it has none. An ollama model is
//! loaded and unloaded through `keep_alive`. A sheep model that names a container has it stopped
//! with `podman stop --ignore` once its sheep has stopped.
//! [`Backends`] takes the [`Model`] the caller holds and never looks a name up in a config, so
//! a model removed from the config since it loaded can still be unloaded.

use core::fmt;
use std::rc::Rc;

use crate::{
    config::{Backend, Model, ModelName, redacted},
    shepherd::{Shepherd, ShepherdError},
};

mod ollama;
mod podman;
mod probe;
mod ready;
mod sheep;

pub(crate) use podman::{Containers, Podman};
pub(crate) use probe::OllamaLoaded;
pub(crate) use ready::wait_ready;

// ollama's `keep_alive`: -1 holds the model in memory until told otherwise, 0 unloads it now.
const KEEP_LOADED: i64 = -1;
const UNLOAD_NOW: i64 = 0;

/// Why a model did not load or unload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// The shepherd failed or refused a request for a sheep.
    Shepherd(ShepherdError),
    /// A request to the backend could not be made or answered.
    Http {
        /// The url requested, redacted.
        url: String,
        /// What went wrong, in the HTTP client's words.
        error: String,
    },
    /// The backend answered a load or unload with a status outside 2xx.
    Status {
        /// The url requested, redacted.
        url: String,
        /// The status code.
        status: u16,
        /// The response body, as the backend sent it.
        body: String,
    },
    /// A sheep load was asked of a model whose backend is not a sheep.
    NotASheep {
        /// The model.
        model: ModelName,
    },
    /// The model has a ready check and no url to ask it at.
    NoUrl {
        /// The model.
        model: ModelName,
    },
    /// The flock showed the sheep stopped or errored before it came online.
    Stopped {
        /// The sheep.
        sheep: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shepherd(err) => write!(f, "{err}"),
            Self::Http { url, error } => write!(f, "request to {url} failed: {error}"),
            Self::Status { url, status, body } => {
                write!(f, "{url} answered {status}: {body}")
            }
            Self::NotASheep { model } => write!(f, "{model} is not served by a sheep"),
            Self::NoUrl { model } => write!(f, "{model} has a ready check and no url"),
            Self::Stopped { sheep } => write!(f, "sheep {sheep} stopped before it came online"),
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Shepherd(err) => Some(err),
            Self::Http { .. }
            | Self::Status { .. }
            | Self::NotASheep { .. }
            | Self::NoUrl { .. }
            | Self::Stopped { .. } => None,
        }
    }
}

impl From<ShepherdError> for LoadError {
    fn from(err: ShepherdError) -> Self {
        Self::Shepherd(err)
    }
}

/// The I/O that puts a model on, or takes it off, the host's GPU.
#[derive(Debug)]
pub(crate) struct Backends<S> {
    shepherd: S,
    http: reqwest::Client,
    containers: Rc<dyn Containers>,
}

impl<S: Shepherd> Backends<S> {
    /// Backends that reach sheep through `shepherd` and ollama through `http`.
    pub(crate) fn new(shepherd: S, http: reqwest::Client) -> Self {
        Self {
            shepherd,
            http,
            containers: Rc::new(Podman),
        }
    }

    /// The same backends, stopping containers through `containers`
    #[cfg(test)]
    pub(crate) fn with_containers(mut self, containers: Rc<dyn Containers>) -> Self {
        self.containers = containers;
        self
    }

    /// The shepherd, for the engine's own requests.
    pub(crate) fn shepherd(&self) -> &S {
        &self.shepherd
    }

    /// Starts serving `model`, returning once it is ready.
    ///
    /// A sheep model is polled until ready, or waited on until its sheep comes
    /// online, without a bound: the caller bounds it with the model's `load_timeout`.
    ///
    /// # Errors
    /// [`LoadError::Shepherd`] when a sheep request fails, [`LoadError::Http`] or
    /// [`LoadError::Status`] when the backend cannot be reached or refuses,
    /// [`LoadError::NoUrl`] when a sheep model's ready check has no url, and
    /// [`LoadError::Stopped`] when a sheep stops before it comes online.
    ///
    /// # Cancellation safety
    /// Dropping the future leaves the sheep's env parked and possibly restarted.
    pub(crate) async fn load(&self, model: &Model) -> Result<(), LoadError> {
        match &model.backend {
            Backend::Sheep { .. } => self.load_sheep(model).await,
            Backend::Ollama { url, name } => {
                ollama::keep_alive(&self.http, url, name, model.key(), KEEP_LOADED).await
            }
        }
    }

    /// Stops serving `model`.
    ///
    /// A sheep model that names a container has it stopped once the sheep has: a container
    /// already gone is stopped, and one that cannot be stopped is logged, since its sheep is.
    ///
    /// # Errors
    /// As [`Self::load`].
    ///
    /// # Cancellation safety
    /// Dropping the future may leave the model loaded, or the sheep stopped without the
    /// caller having seen it.
    pub(crate) async fn unload(&self, model: &Model) -> Result<(), LoadError> {
        match &model.backend {
            Backend::Sheep { sheep, .. } => {
                self.shepherd.stop(sheep).await?;
                if let Some(container) = &model.container
                    && let Err(err) = self.containers.stop(container).await
                {
                    eprintln!(
                        "paddock: stopping container {container} after {} failed: {err}",
                        model.name
                    );
                }
                Ok(())
            }
            Backend::Ollama { url, name } => {
                ollama::keep_alive(&self.http, url, name, model.key(), UNLOAD_NOW).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Backends, redacted};
    use crate::test_support::{FakeContainers, FakeShepherd, model};

    #[tokio::test(start_paused = true)]
    async fn an_unload_whose_container_will_not_stop_still_finishes() {
        let shepherd = FakeShepherd::new();
        let containers = FakeContainers::after(&shepherd).failing();
        let backends = Backends::new(shepherd, crate::outbound::http_client())
            .with_containers(Rc::new(containers.clone()));
        let mut strata = model("iq3_s");
        strata.container = Some("strata-qwen-iq3_s".to_owned());

        assert_eq!(backends.unload(&strata).await, Ok(()));
        assert_eq!(
            containers.stopped(),
            vec![("strata-qwen-iq3_s".to_owned(), 1)]
        );
        assert_eq!(backends.unload(&model("laya")).await, Ok(()));
        assert_eq!(
            containers.stopped().len(),
            1,
            "a model with no container stops none"
        );
    }

    #[test]
    fn userinfo_query_and_fragment_go_and_the_rest_stays() {
        for (given, kept) in [
            ("http://u:p@host:1/api?token=t#f", "http://host:1/api"),
            ("http://host/a#t", "http://host/a"),
            ("http://u@host", "http://host"),
            ("http://host:1/a@b", "http://host:1/a@b"),
            ("host/a", "host/a"),
            ("user:s3cret@host:1/a", "host:1/a"),
            ("user@host", "host"),
            ("host/a@b", "host/a@b"),
            ("//user:s3cret@host:1/a?k=t", "//host:1/a"),
        ] {
            assert_eq!(redacted(given), kept, "{given}");
        }
    }
}
