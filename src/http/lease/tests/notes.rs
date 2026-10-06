//! Notes, idle ends and reclaimed ends over a real loopback socket. Real time throughout: a
//! ttl and an idle time pass on the engine's clock, which a real socket keeps real. Every
//! await is bounded by `LIMIT`.

use super::*;

async fn put(paddock: &Paddock, id: &str, key: &str, body: Option<&str>) -> reqwest::Response {
    paddock
        .send(
            reqwest::Method::PUT,
            &format!("/paddock/leases/{id}"),
            key,
            body,
        )
        .await
}

/// The first lease in the status, as `key`'s client sees it.
async fn first_lease(paddock: &Paddock, key: &str) -> Value {
    let (_, status) = json_of(
        paddock
            .send(reqwest::Method::GET, "/paddock/status", key, None)
            .await,
    )
    .await;
    status["leases"][0].clone()
}

/// The next line that is not a heartbeat.
async fn next_said(lines: &mut Lines) -> Value {
    loop {
        let line = lines.next_line().await.expect("the stream is open");
        if line.get("heartbeat").is_none() {
            return line;
        }
    }
}

/// Reads up to the grant, failing on anything but a wait before it.
async fn until_granted(lines: &mut Lines) {
    loop {
        let line = next_said(lines).await;
        if line.get("granted").is_some() {
            return;
        }
        assert!(
            line.get("queued").is_some(),
            "neither queued nor granted: {line}"
        );
    }
}

#[tokio::test]
async fn a_note_renews_a_heartbeat_lease_and_shows_in_the_status() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let taken = paddock
            .take(
                "k-mac",
                r#"{"model":"iq2_xs","hold":"heartbeat","ttl":"2s"}"#,
            )
            .await;
        let (code, granted) = json_of(taken).await;
        assert_eq!(code, 200, "{granted}");
        let id = granted["id"].as_str().expect("an id").to_owned();

        sleep(Duration::from_millis(1_200)).await;
        let noted = put(&paddock, &id, "k-mac", Some(r#"{"note":"step 412/900"}"#)).await;
        assert_eq!(noted.status().as_u16(), 204);
        sleep(Duration::from_millis(1_200)).await;
        let renewed = put(&paddock, &id, "k-mac", None).await;
        assert_eq!(renewed.status().as_u16(), 204, "the note renewed it");
        assert_eq!(
            first_lease(&paddock, "k-mac").await["note"],
            json!("step 412/900")
        );
    })
    .await;
}

#[tokio::test]
async fn a_put_of_an_empty_object_renews_as_slice_1_did() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let taken = paddock
            .take(
                "k-mac",
                r#"{"model":"iq2_xs","hold":"heartbeat","ttl":"2s"}"#,
            )
            .await;
        let (code, granted) = json_of(taken).await;
        assert_eq!(code, 200, "{granted}");
        let id = granted["id"].as_str().expect("an id").to_owned();

        sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            put(&paddock, &id, "k-mac", Some("{}"))
                .await
                .status()
                .as_u16(),
            204
        );
        sleep(Duration::from_millis(1_200)).await;
        let renewed = put(&paddock, &id, "k-mac", None).await;
        assert_eq!(
            renewed.status().as_u16(),
            204,
            "the empty object renewed it"
        );
        assert_eq!(first_lease(&paddock, "k-mac").await["note"], json!(null));
    })
    .await;
}

#[tokio::test]
async fn a_note_on_a_connection_lease_is_a_note_only() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_lines, id) = paddock.held("iq2_xs").await;
        let noted = put(&paddock, &id, "k-mac", Some(r#"{"note":"warming up"}"#)).await;
        assert_eq!(noted.status().as_u16(), 204);
        assert_eq!(
            first_lease(&paddock, "k-mac").await["note"],
            json!("warming up")
        );
    })
    .await;
}

#[tokio::test]
async fn a_note_over_1024_bytes_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_lines, id) = paddock.held("iq2_xs").await;
        let body = json!({ "note": "x".repeat(1_025) }).to_string();
        let (code, answer) = json_of(put(&paddock, &id, "k-mac", Some(&body)).await).await;
        assert_eq!(
            (code, &answer["error"]),
            (400, &json!("note_too_long")),
            "{answer}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_put_with_any_other_body_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_lines, id) = paddock.held("iq2_xs").await;
        for body in [r#"{"notes":"x"}"#, r#"{"note":7}"#, "not json"] {
            let (code, answer) = json_of(put(&paddock, &id, "k-mac", Some(body)).await).await;
            assert_eq!(
                (code, &answer["error"]),
                (400, &json!("bad_lease_request")),
                "{body}: {answer}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_note_on_another_clients_lease_is_404() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let (_lines, id) = paddock.held("iq2_xs").await;
        let noted = put(&paddock, &id, "k-bench", Some(r#"{"note":"x"}"#)).await;
        assert_eq!(noted.status().as_u16(), 404);
    })
    .await;
}

#[tokio::test]
async fn a_heartbeat_lease_released_for_idleness_answers_its_next_renewal_404() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let body = r#"{"model":"iq2_xs","hold":"heartbeat","ttl":"60s","release_if_idle":"1s"}"#;
        let (code, granted) = json_of(paddock.take("k-mac", body).await).await;
        assert_eq!(code, 200, "{granted}");
        let id = granted["id"].as_str().expect("an id").to_owned();
        sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            put(&paddock, &id, "k-mac", None).await.status().as_u16(),
            404
        );
    })
    .await;
}

#[tokio::test]
async fn an_empty_put_renews_without_counting_as_activity() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let body = r#"{"model":"iq2_xs","hold":"heartbeat","ttl":"60s","release_if_idle":"2s"}"#;
        let (code, granted) = json_of(paddock.take("k-mac", body).await).await;
        assert_eq!(code, 200, "{granted}");
        let id = granted["id"].as_str().expect("an id").to_owned();

        sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            put(&paddock, &id, "k-mac", None).await.status().as_u16(),
            204
        );
        // 2.6s after the grant but 1.4s after the renewal: idle only if the renewal was not use.
        sleep(Duration::from_millis(1_400)).await;
        assert_eq!(
            put(&paddock, &id, "k-mac", None).await.status().as_u16(),
            404
        );
    })
    .await;
}

#[tokio::test]
async fn a_connection_lease_released_for_idleness_says_so_on_its_stream() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let mut lines = Lines::from(
            paddock
                .take("k-mac", r#"{"model":"iq2_xs","release_if_idle":"1s"}"#)
                .await,
        );
        until_granted(&mut lines).await;
        assert_eq!(
            next_said(&mut lines).await,
            json!({ "ended": { "reason": "idle", "idle_for": "1s" } })
        );
    })
    .await;
}

#[tokio::test]
async fn a_reclaimable_lease_ends_reclaimed_on_its_stream() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let mut kept = Lines::from(
            paddock
                .take("k-mac", r#"{"model":"iq2_xs","reclaimable":true}"#)
                .await,
        );
        until_granted(&mut kept).await;
        let mut wanted = Lines::from(
            paddock
                .take("k-bench", r#"{"model":"iq3_s","priority":"interactive"}"#)
                .await,
        );

        assert_eq!(
            next_said(&mut kept).await,
            json!({ "ended": { "reason": "reclaimed" } })
        );
        until_granted(&mut wanted).await;
    })
    .await;
}

#[tokio::test]
async fn a_release_if_idle_outside_sheps_grammar_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let body = r#"{"model":"iq2_xs","release_if_idle":"half an hour"}"#;
        let (code, answer) = json_of(paddock.take("k-mac", body).await).await;
        assert_eq!(code, 400);
        assert_eq!(
            answer,
            json!({
                "error": "bad_lease_request",
                "detail": "release_if_idle is not a duration such as 30s or 8h",
            })
        );
    })
    .await;
}

#[tokio::test]
async fn a_release_if_idle_of_zero_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        for zero in ["0", "0s", "0ms"] {
            let body = json!({ "model": "iq2_xs", "release_if_idle": zero }).to_string();
            let (code, answer) = json_of(paddock.take("k-mac", &body).await).await;
            assert_eq!(code, 400, "{zero}");
            assert_eq!(
                answer,
                json!({
                    "error": "bad_lease_request",
                    "detail": "release_if_idle must be more than 0",
                }),
                "{zero}"
            );
        }
    })
    .await;
}
