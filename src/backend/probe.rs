//! Asking a backend once what it has loaded, for discovery at start.

use core::time::Duration;

use serde::Deserialize;
use tokio::time::timeout;

use super::{Backends, LoadError, ready::is_ready};
use crate::{config::Model, shepherd::Shepherd};

// One answer on loopback takes milliseconds. A backend silent this long is hung,
// and the dog does not listen until discovery ends.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What ollama's `/api/ps` answers, read only as far as discovery needs.
#[derive(Debug, Deserialize)]
struct Ps {
    models: Vec<PsModel>,
}

#[derive(Debug, Deserialize)]
struct PsModel {
    name: String,
}

impl<S: Shepherd> Backends<S> {
    /// Whether `model`'s ready check passes now, asked once
    ///
    /// A model with no ready check passes, as its load does. No answer
    /// within a few seconds fails.
    pub(crate) async fn ready_now(&self, model: &Model) -> bool {
        let Some(ready) = &model.ready else {
            return true;
        };
        let base = model.url.as_deref().unwrap_or_default();
        let asked = is_ready(&self.http, base, ready, model.key());
        matches!(timeout(PROBE_TIMEOUT, asked).await, Ok(Ok(true)))
    }

    /// The names the ollama at `url` has loaded, from its `/api/ps`
    ///
    /// # Errors
    /// [`LoadError::Http`] when ollama cannot be reached, does not answer within
    /// a few seconds, or answers with a body that is not a model list.
    /// [`LoadError::Status`] when it answers outside 2xx.
    pub(crate) async fn ollama_loaded(
        &self,
        url: &str,
        key: Option<&str>,
    ) -> Result<Vec<String>, LoadError> {
        let target = format!("{url}/api/ps");
        let http_error = |error: String| LoadError::Http {
            url: target.clone(),
            error,
        };
        let mut request = self.http.get(&target);
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let asked = async {
            let response = request.send().await?;
            let status = response.status();
            let body = response.text().await?;
            Ok::<_, reqwest::Error>((status, body))
        };
        let (status, body) = match timeout(PROBE_TIMEOUT, asked).await {
            Ok(Ok(answered)) => answered,
            Ok(Err(err)) => return Err(http_error(err.without_url().to_string())),
            Err(_) => return Err(http_error(format!("no answer within {PROBE_TIMEOUT:?}"))),
        };
        if !status.is_success() {
            return Err(LoadError::Status {
                url: target,
                status: status.as_u16(),
                body,
            });
        }
        let ps: Ps = serde_json::from_str(&body).map_err(|err| http_error(err.to_string()))?;
        Ok(ps.models.into_iter().map(|model| model.name).collect())
    }
}
