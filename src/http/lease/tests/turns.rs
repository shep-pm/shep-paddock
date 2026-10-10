//! A lease on a model whose backend serves a limited number of leases at once.

use super::*;

/// iq3_s serves one lease at a time, so bench-01's lease waits for mac-sessions' turn to end.
#[tokio::test]
async fn a_lease_past_its_models_turns_streams_a_turn_reason() {
    let iq3_s = "url = \"http://127.0.0.1:8081\"\nvram = \"all\"";
    assert!(TWO_CLIENTS.contains(iq3_s), "iq3_s's section moved");
    let one_turn = config(&TWO_CLIENTS.replace(iq3_s, &format!("{iq3_s}\nsequences = 1")));
    with_paddock_timed(
        one_turn,
        FakeShepherd::new(),
        Timeouts::default(),
        |paddock| async move {
            let (_held, _id) = paddock.held("iq3_s").await;

            let body = json!({ "model": "iq3_s", "note": "#80" }).to_string();
            let mut response = Lines::from(paddock.take("k-bench", &body).await);

            let queued = response.next_line().await.expect("a queued line");
            assert_eq!(
                queued["queued"]["reason"], "iq3_s is serving mac-sessions, next in line",
                "{queued}"
            );
            assert_eq!(queued["queued"]["reason_kind"], "turn", "{queued}");
        },
    )
    .await;
}
