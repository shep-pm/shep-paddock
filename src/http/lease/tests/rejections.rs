//! Requests the lease routes turn away.

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::*;
use crate::http::proxy::MAX_BODY;

/// Sends `head`, which ends the request head, and returns everything the endpoint answers.
async fn raw_post(paddock: &Paddock, head: &str) -> String {
    let head = format!(
        "POST /paddock/leases HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-mac\r\n{head}\r\n"
    );
    let mut stream = bounded("connect", TcpStream::connect(paddock.addr))
        .await
        .expect("connect");
    stream.write_all(head.as_bytes()).await.expect("write");
    let mut answer = String::new();
    bounded("the answer", stream.read_to_string(&mut answer))
        .await
        .expect("read");
    answer
}

#[tokio::test]
async fn an_unknown_field_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        for body in [
            r#"{"model":"iq2_xs","hodl":"connection"}"#,
            r#"{"model":"iq2_xs","hold":"forever"}"#,
            r#"{"model":"iq2_xs","priority":"urgent"}"#,
            r#"{"model":"iq2_xs","ttl":"1.5s"}"#,
            r#"{"model":"iq2_xs","expected":"8 hours"}"#,
            r#"{"priority":"batch"}"#,
            "not json",
        ] {
            let (status, body_json) = json_of(paddock.take("k-mac", body).await).await;
            assert_eq!(
                (status, body_json["error"].clone()),
                (400, json!("bad_lease_request")),
                "{body}"
            );
        }
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}

#[tokio::test]
async fn an_unknown_model_is_404() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (status, body) = json_of(paddock.take("k-mac", r#"{"model":"nope"}"#).await).await;

        assert_eq!(status, 404);
        assert_eq!(body["error"], "unknown_model");
    })
    .await;
}

#[tokio::test]
async fn a_bad_lease_id_is_404() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        for id in [
            "7",
            "L",
            "L07",
            "L+7",
            "Lx",
            "L7x",
            "L99999999999999999999",
            "l7",
        ] {
            for (method, path) in [
                (reqwest::Method::PUT, format!("/paddock/leases/{id}")),
                (reqwest::Method::DELETE, format!("/paddock/leases/{id}")),
                (
                    reqwest::Method::POST,
                    format!("/paddock/leases/{id}/attach"),
                ),
            ] {
                assert_eq!(
                    paddock.status(method.clone(), &path, "k-mac").await,
                    404,
                    "{method} {path}"
                );
            }
        }
        assert_eq!(
            paddock
                .status(reqwest::Method::PUT, "/paddock/leases/L999", "k-mac")
                .await,
            404
        );
    })
    .await;
}

#[tokio::test]
async fn the_routes_need_a_key() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let status = paddock
            .status(reqwest::Method::DELETE, "/paddock/leases/L1", "wrong")
            .await;

        assert_eq!(status, 401);
    })
    .await;
}

#[tokio::test]
async fn a_path_that_only_starts_with_the_lease_prefix_is_not_a_lease_route() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_open, id) = paddock.held("iq2_xs").await;
        let (_, body) = json_of(
            paddock
                .take("k-mac", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                .await,
        )
        .await;
        let beating = body["id"].as_str().expect("an id").to_owned();

        for lease in [&id, &beating] {
            for (method, path) in [
                (reqwest::Method::PUT, format!("/paddock/leasesX/{lease}")),
                (reqwest::Method::DELETE, format!("/paddock/leasesX/{lease}")),
                (
                    reqwest::Method::POST,
                    format!("/paddock/leasesX/{lease}/attach"),
                ),
                (reqwest::Method::POST, "/paddock/leasesX".to_owned()),
            ] {
                assert_eq!(
                    paddock.status(method.clone(), &path, "k-mac").await,
                    404,
                    "{method} {path}"
                );
            }
        }
        let snapshot = paddock.engine.snapshot().await;
        assert_eq!(snapshot.leases.len(), 2);
        assert!(snapshot.leases.iter().all(|lease| lease.attached));
    })
    .await;
}

#[tokio::test]
async fn an_oversized_lease_body_is_413() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        // The length alone says it is too large, so none of the body is sent.
        let answer = raw_post(
            &paddock,
            &format!("Content-Length: {}\r\nConnection: close\r\n", MAX_BODY + 1),
        )
        .await;

        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        assert!(
            answer.ends_with(r#"{"error":"body_too_large"}"#),
            "{answer}"
        );
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_lease_body_that_stops_arriving_is_408() {
    let timeouts = Timeouts {
        body_read: Duration::from_millis(200),
        ..Timeouts::default()
    };
    with_paddock_timed(FakeShepherd::new(), timeouts, |paddock| async move {
        // Ten bytes declared and four sent, with the line break raw_post adds, so the body never ends.
        let answer = raw_post(&paddock, "Content-Length: 10\r\n\r\n{}").await;

        assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        assert!(answer.ends_with(r#"{"error":"body_timeout"}"#), "{answer}");
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}
