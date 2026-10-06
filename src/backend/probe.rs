//! Asking a backend once what it has loaded, for discovery at start.

use core::time::Duration;

use serde::Deserialize;
use tokio::time::timeout;

use super::{Backends, LoadError, ready::is_ready, redacted};
use crate::{
    config::Model,
    footprint::{Footprint, Vram},
    shepherd::Shepherd,
};

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
    /// Bytes held in all, VRAM included.
    #[serde(default)]
    size: u64,
    #[serde(default)]
    size_vram: u64,
}

/// One model ollama has loaded, and what it reports holding
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OllamaLoaded {
    /// The name ollama lists it under.
    pub name: String,
    /// `size_vram` as VRAM, and the rest of `size` as RAM.
    pub footprint: Footprint,
}

/// The model blob a `/api/show` modelfile loads from: its first `FROM` line naming a blob
///
/// `None` when no `FROM` line's path ends in `sha256-<hex>`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the engine's survey is its caller")
)]
pub(crate) fn blob_of(modelfile: &str) -> Option<String> {
    modelfile
        .lines()
        .filter_map(|line| line.strip_prefix("FROM "))
        .find_map(|path| {
            path.trim()
                .rsplit('/')
                .next()?
                .strip_prefix("sha256-")
                .filter(|hex| !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .map(str::to_owned)
        })
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

    /// What the ollama at `url` has loaded, from its `/api/ps`
    ///
    /// # Errors
    /// [`LoadError::Http`] when ollama cannot be reached, does not answer within
    /// a few seconds, or answers with a body that is not a model list.
    /// [`LoadError::Status`] when it answers outside 2xx.
    pub(crate) async fn ollama_loaded(
        &self,
        url: &str,
        key: Option<&str>,
    ) -> Result<Vec<OllamaLoaded>, LoadError> {
        let target = format!("{url}/api/ps");
        let http_error = |error: String| LoadError::Http {
            url: redacted(&target),
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
                url: redacted(&target),
                status: status.as_u16(),
                body,
            });
        }
        let ps: Ps = serde_json::from_str(&body).map_err(|err| http_error(err.to_string()))?;
        let loaded = ps.models.into_iter().map(|model| OllamaLoaded {
            footprint: Footprint {
                vram: Vram::Bytes(model.size_vram),
                ram: model.size.saturating_sub(model.size_vram),
            },
            name: model.name,
        });
        Ok(loaded.collect())
    }
}

#[cfg(test)]
mod tests {
    use super::blob_of;
    use crate::test_support::captured::{QWEN_BLOB, SHOW_QWEN};

    #[test]
    fn the_captured_modelfile_names_the_runners_blob() {
        let show: serde_json::Value = serde_json::from_str(SHOW_QWEN).expect("json");
        let modelfile = show["modelfile"].as_str().expect("a modelfile");
        assert_eq!(blob_of(modelfile).as_deref(), Some(QWEN_BLOB));
    }

    /// Built around the captured FROM line: `ollama show` prints a commented FROM naming the model
    /// above the one naming the blob.
    #[test]
    fn the_blob_is_the_first_from_line_that_names_one() {
        let modelfile = format!(
            "# FROM qwen3.8:27b-ctx65536\n\nFROM qwen3.8:27b\nFROM /home/<user>/.ollama/models/blobs/sha256-{QWEN_BLOB}\n"
        );
        assert_eq!(blob_of(&modelfile).as_deref(), Some(QWEN_BLOB));
        assert_eq!(blob_of(""), None);
        assert_eq!(blob_of("FROM /b/sha256-\nFROM /b/sha256-xyz\n"), None);
    }
}
