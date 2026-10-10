//! Revoke over a real loopback socket, so on real time; every await is bounded by `LIMIT`.

use super::*;

/// [`TWO_CLIENTS`] with mac-sessions an admin.
fn with_admin() -> Arc<Config> {
    config(&TWO_CLIENTS.replace("key = \"k-mac\"", "key = \"k-mac\"\nadmin = true"))
}

/// [`with_admin`]'s config with bench-01 protected, and marked `bench` besides.
fn with_bench_protected(bench: &str) -> Arc<Config> {
    let marked = format!("key = \"k-bench\"\nprotected = true\n{bench}");
    let text = TWO_CLIENTS
        .replace("key = \"k-mac\"", "key = \"k-mac\"\nadmin = true")
        .replace("key = \"k-bench\"", &marked);
    config(&text)
}

async fn revoke(paddock: &Paddock, key: &str, id: &str, body: Option<&str>) -> reqwest::Response {
    let path = format!("/paddock/leases/{id}/revoke");
    paddock.send(reqwest::Method::POST, &path, key, body).await
}

/// Reads `response` up to its grant and returns the lease id.
async fn granted_id(response: &mut Lines) -> String {
    loop {
        let line = response.next_line().await.expect("a line");
        if let Some(granted) = line.get("granted") {
            return granted["id"].as_str().expect("an id").to_owned();
        }
    }
}

fn admin_paddock<F, Fut>(body: F) -> impl Future<Output = ()>
where
    F: FnOnce(Paddock) -> Fut,
    Fut: Future<Output = ()>,
{
    with_paddock_timed(with_admin(), FakeShepherd::new(), Timeouts::default(), body)
}

#[tokio::test]
async fn only_an_admin_may_revoke_and_the_heartbeat_holders_next_renewal_is_404() {
    admin_paddock(|paddock| async move {
        let (_, body) = json_of(
            paddock
                .take("k-bench", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                .await,
        )
        .await;
        let id = body["id"].as_str().expect("an id").to_owned();

        let (status, answer) = json_of(revoke(&paddock, "k-bench", &id, None).await).await;
        assert_eq!((status, answer), (403, json!({ "error": "forbidden" })));
        assert_eq!(
            revoke(&paddock, "k-mac", &id, None).await.status().as_u16(),
            204
        );

        let renewal = paddock
            .status(
                reqwest::Method::PUT,
                &format!("/paddock/leases/{id}"),
                "k-bench",
            )
            .await;
        assert_eq!(renewal, 404);
    })
    .await;
}

#[tokio::test]
async fn revoking_an_ended_or_unknown_lease_is_404() {
    admin_paddock(|paddock| async move {
        for id in ["L99", "nonsense"] {
            let (status, answer) = json_of(revoke(&paddock, "k-mac", id, None).await).await;
            assert_eq!(
                (status, answer),
                (404, json!({ "error": "not_found" })),
                "{id}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_revoked_model_lease_ends_its_stream_naming_who_and_why() {
    admin_paddock(|paddock| async move {
        let mut held = Lines::from(paddock.take("k-bench", r#"{"model":"iq2_xs"}"#).await);
        let id = granted_id(&mut held).await;

        let answer = revoke(&paddock, "k-mac", &id, Some(r#"{"reason":"forgotten since Tuesday"}"#)).await;
        assert_eq!(answer.status().as_u16(), 204);

        let ended = held.next_line().await.expect("an ended line");
        assert_eq!(
            ended,
            json!({ "ended": { "reason": "revoked", "by": "mac-sessions", "note": "forgotten since Tuesday" } })
        );
        assert_eq!(held.next_line().await, None, "the body did not end");
    })
    .await;
}

#[tokio::test]
async fn a_revoked_bare_lease_keeps_its_memory_until_its_holder_hangs_up() {
    admin_paddock(|paddock| async move {
        let mut held = Lines::from(
            paddock
                .take("k-bench", r#"{"footprint":{"vram":"20G","ram":"1G"}}"#)
                .await,
        );
        let id = granted_id(&mut held).await;

        assert_eq!(
            revoke(&paddock, "k-mac", &id, None).await.status().as_u16(),
            204
        );
        let ended = held.next_line().await.expect("an ended line");
        assert_eq!(
            ended,
            json!({ "ended": { "reason": "revoked", "by": "mac-sessions", "note": null } })
        );
        let snapshot = paddock.engine.snapshot().await;
        assert_eq!(
            snapshot.declared.ram,
            1 << 30,
            "counted while the job may run"
        );
        assert!(snapshot.leases[0].revoked.is_some());

        drop(held);
        bounded("the memory freed", async {
            while paddock.engine.snapshot().await.declared.ram != 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_revoke_body_is_a_reason_of_at_most_1024_bytes() {
    admin_paddock(|paddock| async move {
        let (_, body) = json_of(
            paddock
                .take("k-bench", r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                .await,
        )
        .await;
        let id = body["id"].as_str().expect("an id").to_owned();
        let long = json!({ "reason": "x".repeat(1_025) }).to_string();
        for (body, error) in [
            (long.as_str(), "reason_too_long"),
            (r#"{"why":"x"}"#, "bad_lease_request"),
        ] {
            let (status, answer) = json_of(revoke(&paddock, "k-mac", &id, Some(body)).await).await;
            assert_eq!((status, answer["error"].clone()), (400, json!(error)));
        }
        assert_eq!(
            paddock.engine.snapshot().await.leases.len(),
            1,
            "a refused revoke ends nothing"
        );
    })
    .await;
}

#[tokio::test]
async fn revoke_takes_only_post() {
    admin_paddock(|paddock| async move {
        let answer = paddock
            .send(
                reqwest::Method::GET,
                "/paddock/leases/L1/revoke",
                "k-mac",
                None,
            )
            .await;
        assert_eq!(answer.status().as_u16(), 405);
        assert_eq!(answer.headers()["allow"], "POST");
    })
    .await;
}

#[tokio::test]
async fn an_admin_may_not_revoke_a_protected_clients_lease() {
    let config = with_bench_protected("");
    with_paddock_timed(
        config,
        FakeShepherd::new(),
        Timeouts::default(),
        |paddock| async move {
            let mut held = Lines::from(paddock.take("k-bench", r#"{"model":"iq2_xs"}"#).await);
            let id = granted_id(&mut held).await;

            let (status, answer) = json_of(revoke(&paddock, "k-mac", &id, None).await).await;
            assert_eq!((status, answer), (403, json!({ "error": "protected" })));
            let leases = paddock.engine.snapshot().await.leases;
            assert_eq!(leases.len(), 1, "the lease is still granted");
            assert_eq!(leases[0].revoked, None);

            let release = paddock
                .status(
                    reqwest::Method::DELETE,
                    &format!("/paddock/leases/{id}"),
                    "k-bench",
                )
                .await;
            assert_eq!(release, 204);
            let ended = held.next_line().await.expect("an ended line");
            assert_eq!(
                ended,
                json!({ "ended": { "reason": "released" } }),
                "no revoked line came first"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_protected_admin_may_revoke_its_own_lease() {
    let config = with_bench_protected("admin = true");
    with_paddock_timed(
        config,
        FakeShepherd::new(),
        Timeouts::default(),
        |paddock| async move {
            let mut held = Lines::from(paddock.take("k-bench", r#"{"model":"iq2_xs"}"#).await);
            let id = granted_id(&mut held).await;

            assert_eq!(
                revoke(&paddock, "k-bench", &id, None)
                    .await
                    .status()
                    .as_u16(),
                204
            );
            let ended = held.next_line().await.expect("an ended line");
            assert_eq!(
                ended,
                json!({ "ended": { "reason": "revoked", "by": "bench-01", "note": null } })
            );
        },
    )
    .await;
}
