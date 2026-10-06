//! Requests answered without a forward: bad bodies and headers, unknown models, failures.

use super::*;

#[tokio::test]
async fn not_json_is_400() {
    assert_eq!(
        refused_at_once("model=iq2_xs", &[]).await,
        (400, json!({"error": "not_json"}))
    );
}

#[tokio::test]
async fn no_model_is_400() {
    for body in [r#"{"messages":[]}"#, r#"{"model":7}"#, r#"["iq2_xs"]"#] {
        assert_eq!(
            refused_at_once(body, &[]).await,
            (400, json!({"error": "no_model"})),
            "{body}"
        );
    }
}

#[tokio::test]
async fn a_malformed_max_wait_header_is_400() {
    for wait in ["2 minutes", "2min", "-1s", "1.5s", ""] {
        assert_eq!(
            refused_at_once(r#"{"model":"iq2_xs"}"#, &[("X-Paddock-Max-Wait", wait)]).await,
            (400, json!({"error": "bad_max_wait"})),
            "{wait:?}"
        );
    }
}

#[tokio::test]
async fn a_priority_other_than_interactive_or_batch_is_400() {
    for priority in ["Batch", "low", "batch,interactive", ""] {
        assert_eq!(
            refused_at_once(r#"{"model":"iq2_xs"}"#, &[("X-Paddock-Priority", priority)]).await,
            (
                400,
                json!({"error": "bad_priority", "allowed": ["interactive", "batch"]})
            ),
            "{priority:?}"
        );
    }
}

#[tokio::test]
async fn interactive_and_batch_are_both_accepted() {
    let (base, server) = fake_http(vec![("POST", "/v1/chat/completions", vec![(200, "{}")])]);
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        for priority in ["interactive", "batch"] {
            let response = paddock
                .post(
                    "/v1/chat/completions",
                    r#"{"model":"iq2_xs"}"#,
                    &[("X-Paddock-Priority", priority)],
                )
                .await;
            assert_eq!(response.status(), 200, "{priority}");
        }
    })
    .await;
    assert_eq!(server.seen().len(), 2);
}

/// Real time with a short body timeout, since the client is a real socket.
#[tokio::test]
async fn a_body_that_stops_arriving_is_408() {
    let (base, server) = fake_http(vec![("POST", "/v1/chat/completions", vec![(200, "{}")])]);
    let shepherd = FakeShepherd::new();
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    let timeouts = Timeouts {
        body_read: Duration::from_millis(200),
        ..Timeouts::default()
    };
    with_paddock_timed(config, shepherd.clone(), timeouts, |paddock| async move {
        // Ten bytes declared and two sent, so the body never ends.
        let head = "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\n\
                    Content-Length: 10\r\n\r\n{}";
        let mut stream = bounded("connect", TcpStream::connect(paddock.addr))
            .await
            .expect("connect");
        stream.write_all(head.as_bytes()).await.expect("write");
        let mut answer = String::new();
        bounded("the answer", stream.read_to_string(&mut answer))
            .await
            .expect("read");

        assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        assert!(answer.ends_with(r#"{"error":"body_timeout"}"#), "{answer}");
    })
    .await;
    assert!(server.seen().is_empty(), "the request was forwarded");
    assert!(shepherd.calls().is_empty(), "the model was loaded");
}

#[tokio::test]
async fn an_oversized_body_is_413() {
    let (base, server) = fake_http(vec![("POST", "/v1/chat/completions", vec![(200, "{}")])]);
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        // The length alone says it is too large, so none of the body is sent.
        let head = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_BODY + 1
        );
        let mut stream = bounded("connect", TcpStream::connect(paddock.addr))
            .await
            .expect("connect");
        stream.write_all(head.as_bytes()).await.expect("write");
        let mut answer = String::new();
        bounded("the answer", stream.read_to_string(&mut answer))
            .await
            .expect("read");

        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        assert!(
            answer.ends_with(r#"{"error":"body_too_large"}"#),
            "{answer}"
        );
    })
    .await;
    assert!(server.seen().is_empty());
}

#[tokio::test]
async fn an_unknown_model_is_404_and_lists_the_models() {
    let (base, _server) = fake_http(Vec::new());
    let models = sheep("iq2_xs", &base, r#"apis = ["openai"]"#) + &sheep("iq3_s", &base, "");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let response = paddock
                .post("/v1/chat/completions", r#"{"model":"ghost"}"#, &[])
                .await;
            assert_eq!(
                json_of(response).await,
                (
                    404,
                    json!({"error": "unknown_model", "model": "ghost", "models": ["iq2_xs", "iq3_s"]})
                )
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_model_without_the_api_is_400() {
    let (base, server) = fake_http(Vec::new());
    let shepherd = FakeShepherd::new();
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, shepherd.clone(), |paddock| async move {
        let response = paddock
            .post("/v1/messages", r#"{"model":"iq2_xs"}"#, &[])
            .await;
        assert_eq!(
            json_of(response).await,
            (
                400,
                json!({"error": "wrong_api", "model": "iq2_xs", "apis": ["openai"]})
            )
        );
    })
    .await;
    assert!(server.seen().is_empty());
    assert!(shepherd.calls().is_empty());
}

#[tokio::test]
async fn an_ollama_route_for_a_model_without_that_api_is_400() {
    let (base, server) = fake_http(Vec::new());
    let shepherd = FakeShepherd::new();
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, shepherd.clone(), |paddock| async move {
        let response = paddock
            .post("/api/chat", r#"{"model":"iq2_xs"}"#, &[])
            .await;
        assert_eq!(
            json_of(response).await,
            (
                400,
                json!({"error": "wrong_api", "model": "iq2_xs", "apis": ["openai"]})
            )
        );
    })
    .await;
    assert!(server.seen().is_empty());
    assert!(shepherd.calls().is_empty());
}

#[tokio::test]
async fn an_ollama_unload_is_refused_without_loading_anything() {
    let (base, server) = fake_http(Vec::new());
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
        for (path, body) in [
            ("/api/generate", r#"{"model":"qwen3.8:27b","keep_alive":0}"#),
            (
                "/api/generate",
                r#"{"model":"qwen3.8:27b","prompt":"","keep_alive":"0s"}"#,
            ),
            (
                "/api/chat",
                r#"{"model":"qwen3.8:27b","messages":[],"keep_alive":"0"}"#,
            ),
        ] {
            let response = paddock.post(path, body, &[]).await;
            assert_eq!(
                json_of(response).await,
                (
                    403,
                    json!({"error": "unload_refused", "model": "qwen3.8:27b"})
                ),
                "{path} {body}"
            );
        }
    })
    .await;
    assert!(server.seen().is_empty());
}

#[test]
fn only_an_empty_request_with_a_zero_keep_alive_asks_to_unload() {
    let asks = |path: &str, body: Value| asks_to_unload(path, &body);
    assert!(asks("/api/generate", json!({"keep_alive": 0})));
    assert!(asks("/api/generate", json!({"keep_alive": "0m"})));
    assert!(!asks("/api/generate", json!({"keep_alive": "5m"})));
    assert!(!asks("/api/generate", json!({})));
    assert!(!asks(
        "/api/generate",
        json!({"prompt": "hi", "keep_alive": 0})
    ));
    assert!(!asks(
        "/api/chat",
        json!({"messages": [{"role": "user"}], "keep_alive": 0})
    ));
    assert!(!asks("/api/embed", json!({"keep_alive": 0})));
}

#[tokio::test]
async fn a_backend_that_cannot_be_reached_is_502() {
    // A port that was just free, so nothing answers on it.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", closed.local_addr().expect("local addr"));
    drop(closed);
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = paddock
            .post("/v1/chat/completions", r#"{"model":"iq2_xs"}"#, &[])
            .await;
        assert_eq!(
            json_of(response).await,
            (502, json!({"error": "unreachable", "model": "iq2_xs"}))
        );
        paddock.until_in_flight("iq2_xs", 0).await;
    })
    .await;
}

#[tokio::test]
async fn a_model_that_fails_to_load_is_502() {
    let (base, server) = fake_http(Vec::new());
    let shepherd = FakeShepherd::refusing_restart("iq2_xs: no such sheep");
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, shepherd, |paddock| async move {
        let response = paddock
            .post("/v1/chat/completions", r#"{"model":"iq2_xs"}"#, &[])
            .await;
        let (status, body) = json_of(response).await;
        assert_eq!(
            (status, &body["error"], &body["model"]),
            (502, &json!("failed"), &json!("iq2_xs"))
        );
        assert!(
            body["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("no such sheep")),
            "{body}"
        );
    })
    .await;
    assert!(server.seen().is_empty());
}

#[tokio::test]
async fn a_request_behind_a_lease_is_busy() {
    let (base, server) = fake_http(Vec::new());
    let models = sheep("iq2_xs", &base, "") + &sheep("iq3_s", &base, r#"apis = ["openai"]"#);
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let ask = LeaseRequest {
                model: "iq2_xs".into(),
                priority: Priority::Batch,
                expected: None,
                max_wait: None,
                hold: Hold::Connection,
                note: None,
                reclaimable: false,
            };
            let mut events = paddock.engine.take_lease("mac-sessions".into(), ask).await;
            loop {
                match bounded("the grant", events.recv()).await {
                    Some(LeaseEvent::Granted { .. }) => break,
                    Some(LeaseEvent::Waiting { .. }) => {}
                    other => panic!("the lease was not granted: {other:?}"),
                }
            }

            let response = paddock
                .post("/v1/chat/completions", r#"{"model":"iq3_s"}"#, &[])
                .await;
            let (status, body) = json_of(response).await;
            assert_eq!((status, &body["error"]), (503, &json!("busy")));
            drop(events);
        },
    )
    .await;
    assert!(server.seen().is_empty());
}
