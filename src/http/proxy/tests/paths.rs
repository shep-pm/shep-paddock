//! Prefixed paths with dots in them: a `..` segment is refused, and anything else reaches the
//! backend as sent.

use super::*;

/// Sends `GET path` as written, past any client-side normalising, and returns the status and body.
async fn raw_get(paddock: &Paddock, path: &str) -> (u16, String) {
    let mut stream = bounded("connect", TcpStream::connect(paddock.addr))
        .await
        .expect("connect");
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.expect("write");
    let mut answer = String::new();
    bounded("the answer", stream.read_to_string(&mut answer))
        .await
        .expect("read");
    let status = answer
        .get(9..12)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in the answer: {answer:?}"));
    let body = answer
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    (status, body)
}

#[tokio::test]
async fn a_dot_dot_segment_in_a_prefixed_path_is_400() {
    let (base, server) = fake_http(vec![("GET", "/x", vec![(200, "climbed")])]);
    let shepherd = FakeShepherd::new();
    let config = laya_with_prefix(&format!("{base}/api"), "/laya");
    with_paddock(config, shepherd.clone(), |paddock| async move {
        for path in [
            "/laya/../x",
            "/laya/v1/..",
            "/laya/%2e%2e/x",
            "/laya/%2E./x",
            "/laya/.%2e/x",
            "/laya/..\\x",
        ] {
            let answer = raw_get(&paddock, path).await;
            assert_eq!(
                answer,
                (400, r#"{"error":"bad_path"}"#.to_owned()),
                "{path}"
            );
        }
    })
    .await;
    assert!(server.seen().is_empty(), "a request was forwarded");
    assert!(shepherd.calls().is_empty(), "the model was loaded");
}

#[tokio::test]
async fn dots_that_do_not_make_a_dot_dot_segment_reach_the_backend() {
    let (base, server) = fake_http(vec![
        ("GET", "/v1/a..b", vec![(200, "{}")]),
        ("GET", "/...", vec![(200, "{}")]),
        ("GET", "/v1/a%2eb", vec![(200, "{}")]),
        ("GET", "/v1/%252e%252e/x", vec![(200, "{}")]),
    ]);
    let config = laya_with_prefix(&base, "/laya");
    with_paddock(config, FakeShepherd::new(), |paddock| async move {
        for path in [
            "/laya/v1/a..b?x=../..",
            "/laya/...",
            "/laya/v1/a%2eb",
            "/laya/v1/%252e%252e/x",
        ] {
            assert_eq!(raw_get(&paddock, path).await.0, 200, "{path}");
        }
    })
    .await;
    let seen: Vec<_> = server
        .seen()
        .into_iter()
        .map(|seen| (seen.path, seen.query))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("/v1/a..b".to_owned(), Some("x=../..".to_owned())),
            ("/...".to_owned(), None),
            ("/v1/a%2eb".to_owned(), None),
            ("/v1/%252e%252e/x".to_owned(), None),
        ]
    );
}
