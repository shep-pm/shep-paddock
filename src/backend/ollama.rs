//! Loading and unloading a model on ollama through `keep_alive`.

use super::LoadError;

/// Posts `keep_alive` for `name` to `{url}/api/generate`, which ollama answers once the
/// model has loaded or unloaded.
///
/// # Errors
/// [`LoadError::Http`] or [`LoadError::Status`].
pub(super) async fn keep_alive(
    http: &reqwest::Client,
    url: &str,
    name: &str,
    key: Option<&str>,
    seconds: i64,
) -> Result<(), LoadError> {
    let target = format!("{url}/api/generate");
    let body = serde_json::json!({ "model": name, "keep_alive": seconds }).to_string();
    let mut request = http
        .post(&target)
        .header("content-type", "application/json")
        .body(body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let http_error = |err: reqwest::Error| LoadError::Http {
        url: target.clone(),
        error: err.without_url().to_string(),
    };
    let response = request.send().await.map_err(http_error)?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.map_err(http_error)?;
    Err(LoadError::Status {
        url: target,
        status: status.as_u16(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        backend::{Backends, LoadError},
        config::Backend,
        test_support::{FakeShepherd, fake_http, model},
    };
    use core::time::Duration;

    fn ollama_model(base: &str) -> crate::config::Model {
        let mut model = model("qwen3.8:27b");
        model.backend = Backend::Ollama {
            url: base.to_owned(),
            name: "qwen3.8:27b-ctx131072".to_owned(),
        };
        model
    }

    fn backends() -> Backends<FakeShepherd> {
        Backends::new(FakeShepherd::new(), crate::outbound::http_client())
    }

    // Real time throughout: the fake server is a real loopback socket.
    #[tokio::test]
    async fn loading_posts_keep_alive_minus_one() {
        let (base, server) = fake_http(vec![("POST", "/api/generate", vec![(200, "{}")])]);
        tokio::time::timeout(
            Duration::from_secs(10),
            backends().load(&ollama_model(&base)),
        )
        .await
        .expect("finishes")
        .expect("loads");
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&seen[0].body).expect("json body");
        assert_eq!(
            body,
            serde_json::json!({"model": "qwen3.8:27b-ctx131072", "keep_alive": -1})
        );
    }

    #[tokio::test]
    async fn unloading_posts_keep_alive_zero() {
        let (base, server) = fake_http(vec![("POST", "/api/generate", vec![(200, "{}")])]);
        tokio::time::timeout(
            Duration::from_secs(10),
            backends().unload(&ollama_model(&base)),
        )
        .await
        .expect("finishes")
        .expect("unloads");
        let body: serde_json::Value =
            serde_json::from_str(&server.seen()[0].body).expect("json body");
        assert_eq!(
            body,
            serde_json::json!({"model": "qwen3.8:27b-ctx131072", "keep_alive": 0})
        );
    }

    #[tokio::test]
    async fn a_500_is_a_load_error_with_the_body() {
        let (base, _server) = fake_http(vec![(
            "POST",
            "/api/generate",
            vec![(500, "out of memory")],
        )]);
        let err = tokio::time::timeout(
            Duration::from_secs(10),
            backends().load(&ollama_model(&base)),
        )
        .await
        .expect("finishes")
        .expect_err("a 500 fails");
        assert_eq!(
            err,
            LoadError::Status {
                url: format!("{base}/api/generate"),
                status: 500,
                body: "out of memory".to_owned(),
            }
        );
    }
}
