//! Requests that reach their backend: streamed, keyed, routed by prefix, and ended.

use super::*;

#[tokio::test]
async fn a_chat_completion_streams_through_unchanged() {
    let (base, chunks) = fake_sse().await;
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let sent = [
            "data: {\"n\":1}\n\n",
            "data: {\"n\":2}\n\n",
            "data: [DONE]\n\n",
        ];
        chunks
            .send(Bytes::from_static(sent[0].as_bytes()))
            .expect("send");
        let response = paddock
            .post(
                "/v1/chat/completions",
                r#"{"model":"iq2_xs","stream":true}"#,
                &[],
            )
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");

        // Each chunk is sent only once the client has the one before, so a buffered body hangs.
        let mut body = response.bytes_stream();
        let (mut seen, mut expected) = (Vec::new(), Vec::new());
        for (index, chunk) in sent.iter().enumerate() {
            if index > 0 {
                chunks
                    .send(Bytes::from_static(chunk.as_bytes()))
                    .expect("send");
            }
            expected.extend_from_slice(chunk.as_bytes());
            while seen.len() < expected.len() {
                let next = bounded("the next chunk", body.next()).await;
                seen.extend_from_slice(&next.expect("more body").expect("a chunk"));
            }
            assert_eq!(seen, expected);
        }
        drop(chunks);
        assert!(bounded("the end", body.next()).await.is_none());
        paddock.until_in_flight("iq2_xs", 0).await;
    })
    .await;
}

/// A backend that answers each request with `head` and `body` as written, so a header hyper's
/// own server would manage can still be sent.
async fn raw_backend(head: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buf = [0_u8; 4096];
                while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let reply = format!("HTTP/1.1 200 OK\r\n{head}\r\n{body}");
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    base
}

#[tokio::test]
async fn hop_by_hop_headers_of_the_backends_response_stay_behind() {
    let base = raw_backend(
        "Content-Length: 2\r\nConnection: close\r\nKeep-Alive: timeout=5\r\nTrailer: x-sum\r\n\
         Upgrade: h2c\r\nProxy-Authenticate: Basic\r\nContent-Type: text/plain\r\nX-Backend: seen\r\n",
        "ok",
    )
    .await;
    let config = paddock_config(&sheep("iq3_s", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = paddock
            .post("/v1/chat/completions", r#"{"model":"iq3_s"}"#, &[])
            .await;
        assert_eq!(response.status(), 200);
        let headers = response.headers().clone();
        for name in ["keep-alive", "trailer", "upgrade", "proxy-authenticate"] {
            assert!(headers.get(name).is_none(), "{name} was passed on");
        }
        assert_eq!(headers["x-backend"], "seen");
        assert_eq!(headers["content-type"], "text/plain");
        assert_eq!(text_of(response).await, (200, "ok".to_owned()));
    })
    .await;
}

#[tokio::test]
async fn an_ollama_chat_is_routed_by_its_model() {
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
                r#"{"model":"qwen3.8:27b","stream":false,"messages":[{"role":"user","content":"hi"}]}"#,
                &[],
            )
            .await;
        assert_eq!(text_of(response).await, (200, r#"{"done":true}"#.to_owned()));

        let forwarded: Vec<_> = server
            .seen()
            .into_iter()
            .filter(|seen| seen.path == "/api/chat")
            .collect();
        assert_eq!(forwarded.len(), 1);
        let body: Value = serde_json::from_str(&forwarded[0].body).expect("a JSON body");
        assert_eq!(
            body,
            json!({
                "model": "qwen3.8:27b-ctx65536",
                "stream": false,
                "messages": [{"role": "user", "content": "hi"}],
            })
        );
    })
    .await;
}

#[tokio::test]
async fn the_client_key_is_replaced_by_the_model_key() {
    let (base, server) = fake_http(vec![
        ("POST", "/v1/systemone", vec![(200, "{}")]),
        ("POST", "/v1/embeddings", vec![(200, "{}")]),
    ]);
    let models = format!(
        r#"
[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
prefix = "/laya"
key = "k-laya"
ram = "5G"
idle = "8h"
{}"#,
        sheep("iq2_xs", &base, r#"apis = ["openai"]"#)
    );
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let keyed = paddock.post("/laya/v1/systemone", "{}", &[]).await;
            assert_eq!(keyed.status(), 200);
            let keyless = paddock
                .post("/v1/embeddings", r#"{"model":"iq2_xs"}"#, &[])
                .await;
            assert_eq!(keyless.status(), 200);

            let sent: Vec<_> = server
                .seen()
                .into_iter()
                .map(|seen| (seen.path, seen.authorization))
                .collect();
            assert_eq!(
                sent,
                vec![
                    ("/v1/systemone".to_owned(), Some("Bearer k-laya".to_owned())),
                    ("/v1/embeddings".to_owned(), None),
                ]
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_prefixed_request_is_routed_and_stripped() {
    let (base, server) = fake_http(vec![
        ("POST", "/v1/systemone", vec![(200, "classified")]),
        ("GET", "/health", vec![(200, "fine")]),
    ]);
    let models = format!(
        r#"
[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
prefix = "/laya"
ram = "5G"
idle = "8h"
"#
    );
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let posted = paddock.post("/laya/v1/systemone", "not json", &[]).await;
            assert_eq!(text_of(posted).await, (200, "classified".to_owned()));
            let got = bounded(
                "a GET",
                paddock
                    .client
                    .get(format!("http://{}/laya/health", paddock.addr))
                    .bearer_auth("k-mac")
                    .send(),
            )
            .await
            .expect("the endpoint answers");
            assert_eq!(text_of(got).await, (200, "fine".to_owned()));

            let sent: Vec<_> = server
                .seen()
                .into_iter()
                .map(|seen| (seen.method, seen.path, seen.body))
                .collect();
            assert_eq!(
                sent,
                vec![
                    ("POST".into(), "/v1/systemone".into(), "not json".into()),
                    ("GET".into(), "/health".into(), String::new()),
                ]
            );
        },
    )
    .await;
}

#[tokio::test]
async fn hanging_up_mid_stream_ends_the_in_flight_request() {
    let (sse, chunks) = fake_sse().await;
    let (base, _server) = fake_http(vec![("POST", "/v1/chat/completions", vec![(200, "{}")])]);
    let shepherd = FakeShepherd::new();
    let models = sheep("iq2_xs", &sse, r#"apis = ["openai"]"#)
        + &sheep("iq3_s", &base, r#"apis = ["openai"]"#);
    with_paddock(
        paddock_config(&models),
        shepherd.clone(),
        |paddock| async move {
            let body = r#"{"model":"iq2_xs","stream":true}"#;
            let head = format!(
                "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let mut stream = bounded("connect", TcpStream::connect(paddock.addr))
                .await
                .expect("connect");
            stream.write_all(head.as_bytes()).await.expect("write");
            chunks
                .send(Bytes::from_static(b"data: one\n\n"))
                .expect("send");
            let mut got = Vec::new();
            bounded("the first event", async {
                while !got.windows(9).any(|window| window == b"data: one") {
                    let mut buffer = [0; 1024];
                    let read = stream.read(&mut buffer).await.expect("read");
                    assert!(read > 0, "closed before the first event");
                    got.extend_from_slice(&buffer[..read]);
                }
            })
            .await;
            assert_eq!(paddock.in_flight("iq2_xs").await, Some(1));

            // The backend's stream stays open: only the hang-up can end the request.
            drop(stream);
            paddock.until_in_flight("iq2_xs", 0).await;
            let response = paddock
                .post("/v1/chat/completions", r#"{"model":"iq3_s"}"#, &[])
                .await;
            assert_eq!(response.status(), 200);
            assert!(shepherd.calls().contains(&Call::Stop("iq2_xs".into())));
            drop(chunks);
        },
    )
    .await;
}

#[tokio::test]
async fn a_prefix_remainder_never_moves_the_forward_to_another_host() {
    let (base, laya) = fake_http(vec![("GET", "/", vec![(200, "laya")])]);
    let (evil, elsewhere) = fake_http(vec![("GET", "/x", vec![(200, "stolen")])]);
    let authority = evil.trim_start_matches("http://");
    let config = laya_with_prefix(&base, "/laya/");
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = get(&paddock, &format!("/laya/@{authority}/x")).await;
        assert_eq!(response.status(), 404);
        let exact = get(&paddock, "/laya/").await;
        assert_eq!(text_of(exact).await, (200, "laya".to_owned()));
    })
    .await;
    assert!(elsewhere.seen().is_empty(), "the key left for another host");
    assert_eq!(laya.seen().len(), 1);
}

#[tokio::test]
async fn a_double_slash_in_the_remainder_stays_a_path() {
    let (base, laya) = fake_http(Vec::new());
    let (evil, elsewhere) = fake_http(vec![("GET", "/x", vec![(200, "stolen")])]);
    let authority = evil.trim_start_matches("http://");
    let config = laya_with_prefix(&base, "/laya");
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = get(&paddock, &format!("/laya//{authority}/x")).await;
        assert_eq!(response.status(), 404, "laya's own 404 for an unknown path");
    })
    .await;
    assert!(elsewhere.seen().is_empty(), "the key left for another host");
    let seen = laya.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, format!("//{authority}/x"));
}

#[tokio::test]
async fn a_query_string_survives() {
    let (base, server) = fake_http(vec![
        ("GET", "/v1/systemone", vec![(200, "{}")]),
        ("POST", "/v1/chat/completions", vec![(200, "{}")]),
    ]);
    let models = format!(
        r#"
[models.laya]
backend = {{ sheep = "laya" }}
url = "{base}"
prefix = "/laya"
ram = "5G"
idle = "8h"
{}"#,
        sheep("iq2_xs", &base, r#"apis = ["openai"]"#)
    );
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let prefixed = get(&paddock, "/laya/v1/systemone?x=1&y=a%20b").await;
            assert_eq!(prefixed.status(), 200);
            let api = paddock
                .post(
                    "/v1/chat/completions?api-version=2",
                    r#"{"model":"iq2_xs"}"#,
                    &[],
                )
                .await;
            assert_eq!(api.status(), 200);
        },
    )
    .await;
    let queries: Vec<_> = server
        .seen()
        .into_iter()
        .map(|seen| (seen.path, seen.query))
        .collect();
    assert_eq!(
        queries,
        vec![
            ("/v1/systemone".to_owned(), Some("x=1&y=a%20b".to_owned())),
            (
                "/v1/chat/completions".to_owned(),
                Some("api-version=2".to_owned())
            ),
        ]
    );
}

#[tokio::test]
async fn a_redirect_reaches_the_client_unfollowed() {
    let (elsewhere, target) = fake_http(vec![
        ("GET", "/moved", vec![(200, "followed")]),
        ("POST", "/moved", vec![(200, "followed")]),
    ]);
    let location = format!("{elsewhere}/moved");
    let base = fake_redirect(&location).await;
    let config = paddock_config(&sheep("iq2_xs", &base, r#"apis = ["openai"]"#));
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        let response = paddock
            .post("/v1/chat/completions", r#"{"model":"iq2_xs"}"#, &[])
            .await;
        assert_eq!(response.status(), 302);
        assert_eq!(response.headers()["location"], location.as_str());
    })
    .await;
    assert!(target.seen().is_empty(), "the redirect was followed");
}
