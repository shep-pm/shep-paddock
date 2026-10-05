//! Polling a backend until it reports its model loaded.

use core::time::Duration;

use super::LoadError;
use crate::config::Ready;

// A model takes seconds to tens of seconds to load, so a second between polls finds it within
// a second of it being ready and costs the backend one cheap request per second.
const READY_POLL: Duration = Duration::from_secs(1);

/// Polls `base` plus the ready path until the backend reports its model loaded.
///
/// Ready is a 2xx answer and, when the check names a field, a body whose top-level field is
/// truthy: `true`, a non-empty string, array or object, or a non-zero number. A refused
/// connection or a failed read counts as not ready, since the backend is still starting. The
/// poll has no bound of its own, so the caller bounds it with `load_timeout`.
///
/// # Errors
/// [`LoadError::Http`] if the url cannot be built, which no later poll would fix.
///
/// # Cancellation safety
/// Safe to drop between polls; it holds nothing.
pub(crate) async fn wait_ready(
    http: &reqwest::Client,
    base: &str,
    ready: &Ready,
    key: Option<&str>,
) -> Result<(), LoadError> {
    let url = format!("{base}{}", ready.path);
    loop {
        let mut request = http.get(&url);
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                if body_is_ready(response, ready.field.as_deref()).await {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(err) if err.is_builder() => {
                return Err(LoadError::Http {
                    url,
                    error: err.without_url().to_string(),
                });
            }
            Err(_) => {}
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

async fn body_is_ready(response: reqwest::Response, field: Option<&str>) -> bool {
    let Some(field) = field else {
        return true;
    };
    let Ok(text) = response.text().await else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|body| body.get(field).map(is_truthy))
        .unwrap_or(false)
}

fn is_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fake_http;
    use core::time::Duration;

    fn ready(field: Option<&str>) -> Ready {
        Ready {
            path: "/health".to_owned(),
            field: field.map(str::to_owned),
        }
    }

    async fn wait(base: &str, ready: &Ready, key: Option<&str>) -> Result<(), LoadError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            wait_ready(&reqwest::Client::new(), base, ready, key),
        )
        .await
        .expect("wait_ready finishes")
    }

    // Real time throughout: the fake server is a real loopback socket.
    #[tokio::test]
    async fn ready_sends_the_model_key() {
        let (base, server) =
            fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":["m"]}"#)])]);
        wait(&base, &ready(Some("loaded")), Some("k-laya"))
            .await
            .expect("ready");
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].authorization.as_deref(), Some("Bearer k-laya"));
    }

    #[tokio::test]
    async fn an_empty_list_is_not_ready() {
        let (base, server) = fake_http(vec![(
            "GET",
            "/health",
            vec![(200, r#"{"loaded":[]}"#), (200, r#"{"loaded":["m"]}"#)],
        )]);
        wait(&base, &ready(Some("loaded")), None)
            .await
            .expect("ready");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn a_non_2xx_is_not_ready() {
        let (base, server) = fake_http(vec![(
            "GET",
            "/health",
            vec![(503, "loading"), (200, "{}")],
        )]);
        wait(&base, &ready(None), None).await.expect("ready");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn no_field_means_2xx_is_ready() {
        let (base, server) = fake_http(vec![("GET", "/health", vec![(204, "")])]);
        wait(&base, &ready(None), None).await.expect("ready");
        assert_eq!(server.seen().len(), 1);
    }
}
