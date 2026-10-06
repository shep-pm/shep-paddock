//! Requests the lease routes turn away.

use super::*;

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
