//! What a forwarded body looks like to its backend: rewritten where it must be, else as sent.

use super::*;

#[tokio::test]
async fn the_model_is_rewritten_for_ollama_and_keep_alive_is_removed() {
    let (base, server) = fake_http(vec![
        ("POST", "/api/generate", vec![(200, "{}")]),
        (
            "POST",
            "/v1/chat/completions",
            vec![(200, r#"{"ok":true}"#)],
        ),
    ]);
    let config = paddock_config(&format!(
        r#"
[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
apis = ["openai"]
vram = "22323M"
idle = "2h"
"#
    ));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = paddock
            .post(
                "/v1/chat/completions",
                r#"{"model":"qwen3.8:27b","keep_alive":"5m","messages":[{"role":"user","keep_alive":1}]}"#,
                &[],
            )
            .await;
        assert_eq!(text_of(response).await, (200, r#"{"ok":true}"#.to_owned()));

        let forwarded: Vec<_> = server
            .seen()
            .into_iter()
            .filter(|seen| seen.path == "/v1/chat/completions")
            .collect();
        assert_eq!(forwarded.len(), 1);
        let body: Value = serde_json::from_str(&forwarded[0].body).expect("a JSON body");
        assert_eq!(
            body,
            json!({
                "model": "qwen3.8:27b-ctx131072",
                "messages": [{"role": "user", "keep_alive": 1}],
            })
        );
    })
    .await;
}

#[tokio::test]
async fn num_ctx_is_removed_for_ollama_and_other_options_stay() {
    let (base, server) = fake_http(vec![
        ("POST", "/api/generate", vec![(200, "{}")]),
        ("POST", "/api/chat", vec![(200, r#"{"done":true}"#)]),
    ]);
    let config = paddock_config(&format!(
        r#"
[backends.ollama]
kind = "ollama"
url = "{base}"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx65536"
apis = ["ollama"]
vram = "19504M"
idle = "2h"
"#
    ));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = paddock
            .post(
                "/api/chat",
                r#"{"model":"qwen3.8:27b","options":{"num_ctx":131072,"num_predict":300}}"#,
                &[],
            )
            .await;
        assert_eq!(text_of(response).await.0, 200);

        let forwarded: Vec<_> = server
            .seen()
            .into_iter()
            .filter(|seen| seen.path == "/api/chat")
            .collect();
        assert_eq!(forwarded.len(), 1);
        let body: Value = serde_json::from_str(&forwarded[0].body).expect("a JSON body");
        assert_eq!(
            body,
            json!({ "model": "qwen3.8:27b-ctx65536", "options": { "num_predict": 300 } })
        );
    })
    .await;
}

#[tokio::test]
async fn an_unchanged_body_is_forwarded_byte_for_byte() {
    let (base, server) = fake_http(vec![("POST", "/v1/messages", vec![(200, "{}")])]);
    let config = paddock_config(&sheep("iq3_s", &base, r#"apis = ["anthropic"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        // Spacing, key order and `1.50` would not survive a parse and a re-serialize.
        let body = "{ \"z\" : 1.50,\n  \"model\":\"iq3_s\" , \"a\": [ ] }";
        let response = paddock.post("/v1/messages", body, &[]).await;
        assert_eq!(response.status(), 200);

        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, body);
    })
    .await;
}
