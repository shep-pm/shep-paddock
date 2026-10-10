//! Where the dog is, and the client key: `$PADDOCK_KEY`, or else the one in shep's secret store.

use core::fmt;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use shep_client::shep_core::{
    paths::ShepPaths,
    secrets::{self, ALL_ENVIRONMENTS, SecretError},
};

#[cfg(test)]
pub(super) mod tests;

/// The secret in shep's store that stands in for an unset `$PADDOCK_KEY`, set for every
/// environment
pub(super) const STORED_KEY: &str = "PADDOCK_KEY";

/// Where the dog listens unless `$PADDOCK_URL` says otherwise
const DEFAULT_URL: &str = "http://127.0.0.1:8700";

/// How long a lease's stream may stay silent before it counts as broken: three of the dog's
/// 15 s heartbeats
const STREAM_SILENCE: Duration = Duration::from_secs(45);

/// How long to wait between attempts to attach to a lease again
const REATTACH: Duration = Duration::from_secs(2);

/// Where the dog is and the key to speak to it with
#[derive(Clone)]
pub(crate) struct Link {
    /// The dog's address, without a trailing slash.
    pub url: String,
    /// The client key.
    pub key: String,
    /// How long to wait between attempts to attach to a lease again.
    pub retry: Duration,
    /// How long a lease's stream may stay silent before it counts as broken.
    pub silence: Duration,
}

impl Link {
    /// The link `$PADDOCK_URL` and the client key name, or `None` without a key
    ///
    /// The key is `$PADDOCK_KEY`, or else the `PADDOCK_KEY` secret in shep's store.
    ///
    /// # Errors
    /// The store's error when `$PADDOCK_KEY` is unset and the store cannot be read.
    pub(crate) fn from_env(
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, SecretError> {
        let key = match env("PADDOCK_KEY").filter(|key| !key.is_empty()) {
            Some(key) => key,
            None => match stored_key(env)? {
                Some(key) => key,
                None => return Ok(None),
            },
        };
        let url = env("PADDOCK_URL")
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_owned());
        Ok(Some(Self {
            url: url.trim_end_matches('/').to_owned(),
            key,
            retry: REATTACH,
            silence: STREAM_SILENCE,
        }))
    }

    /// A request to `path` on the dog, carrying the key
    pub(super) fn request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.key)
    }
}

// The key is left out, so a `{:?}` in a log line cannot leak it.
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// The `PADDOCK_KEY` secret in the store under `$SHEP_HOME`, or `~/.shep` as shep defaults
///
/// # Errors
/// The store's error when it exists and cannot be read.
fn stored_key(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<String>, SecretError> {
    let home = env("HOME").map(PathBuf::from);
    if home.is_none() && env("SHEP_HOME").is_none() {
        return Ok(None);
    }
    let paths = ShepPaths::resolve(env, home.as_deref().unwrap_or(Path::new("")));
    Ok(secrets::get(&paths.secrets, STORED_KEY, ALL_ENVIRONMENTS)?.filter(|key| !key.is_empty()))
}
