//! Requests for a model whose backend serves a limited number of conversations at once.

use super::*;

/// Sends a chat request for `iq2_xs` as `mac-sessions`, and returns the status once the body ends.
async fn chat(client: reqwest::Client, addr: SocketAddr) -> u16 {
    let response = bounded(
        "the response head",
        client
            .post(format!("http://{addr}/v1/chat/completions"))
            .bearer_auth("k-mac")
            .body(r#"{"model":"iq2_xs","stream":true}"#)
            .send(),
    )
    .await
    .expect("the endpoint answers");
    let status = response.status().as_u16();
    bounded("the body", response.bytes())
        .await
        .expect("the body");
    status
}

/// iq2_xs has one turn, so the lease mac-sessions holds gives its two requests one conversation:
/// the second waits, and the status says it waits for a turn, until the first ends.
#[tokio::test]
async fn two_concurrent_requests_under_one_lease_show_one_in_flight_and_one_waiter() {
    let (sse, chunks) = fake_sse().await;
    let models = sheep("iq2_xs", &sse, "apis = [\"openai\"]\nsequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let ask = LeaseRequest {
                leased: Leased::Model("iq2_xs".into()),
                priority: Priority::Interactive,
                expected: None,
                max_wait: None,
                hold: Hold::Connection,
                note: None,
                reclaimable: false,
                release_if_idle: None,
            };
            let mut events = paddock.engine.take_lease("mac-sessions".into(), ask).await;
            loop {
                match bounded("the grant", events.recv()).await {
                    Some(LeaseEvent::Granted { .. }) => break,
                    Some(LeaseEvent::Waiting { .. }) => {}
                    other => panic!("the lease was not granted: {other:?}"),
                }
            }

            let first = tokio::task::spawn_local(chat(paddock.client.clone(), paddock.addr));
            paddock.until_in_flight("iq2_xs", 1).await;
            let second = tokio::task::spawn_local(chat(paddock.client.clone(), paddock.addr));
            bounded("the second request queueing", async {
                while paddock.engine.snapshot().await.waiters.is_empty() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await;

            let (status, body) = json_of(get(&paddock, "/paddock/status").await).await;
            assert_eq!(status, 200, "{body}");
            let waiters = body["waiters"].as_array().expect("a waiters list");
            assert_eq!(waiters.len(), 1, "{body}");
            assert_eq!(waiters[0]["kind"], "request", "{body}");
            assert_eq!(waiters[0]["reason_kind"], "turn", "{body}");
            assert_eq!(paddock.in_flight("iq2_xs").await, Some(1));

            // The first request's stream ends, which frees the turn for the second.
            drop(chunks);
            assert_eq!(bounded("the first", first).await.expect("joined"), 200);
            assert_eq!(bounded("the second", second).await.expect("joined"), 200);
            drop(events);
        },
    )
    .await;
}
