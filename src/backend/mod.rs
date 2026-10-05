//! Loading and unloading a model on the backend that serves it.
//!
//! A sheep model is loaded by parking its env and args on the sheep, restarting it and waiting
//! for its ready check. An ollama model is loaded and unloaded through `keep_alive`.
//! [`Backends`] takes the [`Model`] the caller holds and never looks a name up in a config, so
//! a model removed from the config since it loaded can still be unloaded.

use core::fmt;

use crate::{
    config::{Backend, Model},
    shepherd::{Shepherd, ShepherdError},
};

mod ollama;
mod probe;
mod ready;
mod sheep;

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
        /// The url requested.
        url: String,
        /// What went wrong, in the HTTP client's words.
        error: String,
    },
    /// The backend answered a load or unload with a status outside 2xx.
    Status {
        /// The url requested.
        url: String,
        /// The status code.
        status: u16,
        /// The response body, as the backend sent it.
        body: String,
    },
    /// The backend never became ready. Only the caller's deadline produces this.
    NotReady {
        /// The url that was polled.
        url: String,
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
            Self::NotReady { url } => write!(f, "{url} did not become ready"),
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Shepherd(err) => Some(err),
            Self::Http { .. } | Self::Status { .. } | Self::NotReady { .. } => None,
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
}

impl<S: Shepherd> Backends<S> {
    /// Backends that reach sheep through `shepherd` and ollama through `http`.
    pub(crate) fn new(shepherd: S, http: reqwest::Client) -> Self {
        Self { shepherd, http }
    }

    /// The shepherd, for the engine's own requests.
    pub(crate) fn shepherd(&self) -> &S {
        &self.shepherd
    }

    /// Starts serving `model`, returning once it is ready.
    ///
    /// A sheep model with a ready check is polled until ready, without a bound: the caller
    /// bounds it with the model's `load_timeout`.
    ///
    /// # Errors
    /// [`LoadError::Shepherd`] when a sheep request fails, [`LoadError::Http`] or
    /// [`LoadError::Status`] when the backend cannot be reached or refuses.
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
    /// # Errors
    /// As [`Self::load`].
    pub(crate) async fn unload(&self, model: &Model) -> Result<(), LoadError> {
        match &model.backend {
            Backend::Sheep { sheep, .. } => Ok(self.shepherd.stop(sheep).await?),
            Backend::Ollama { url, name } => {
                ollama::keep_alive(&self.http, url, name, model.key(), UNLOAD_NOW).await
            }
        }
    }
}
