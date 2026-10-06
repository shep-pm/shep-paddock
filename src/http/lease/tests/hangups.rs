//! A client that hangs up while its lease still queues leaves the queue.

use super::*;

async fn waiters(engine: &EngineHandle) -> usize {
    engine.snapshot().await.waiters.len()
}

async fn until_waiters(engine: &EngineHandle, count: usize) {
    bounded(&format!("{count} waiters"), async {
        while waiters(engine).await != count {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_connection_lease_that_hangs_up_while_queued_leaves_the_queue() {
    // Gated and never opened, so the model never loads and the lease never grants.
    with_paddock(FakeShepherd::gated_restart(), |paddock| async move {
        let mut response = Lines::from(paddock.take("k-mac", r#"{"model":"iq2_xs"}"#).await);
        let queued = response.next_line().await.expect("a queued line");
        assert!(queued.get("queued").is_some(), "{queued}");
        until_waiters(&paddock.engine, 1).await;

        drop(response);

        until_waiters(&paddock.engine, 0).await;
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_heartbeat_lease_that_hangs_up_while_queued_leaves_the_queue() {
    with_paddock(FakeShepherd::gated_restart(), |paddock| async move {
        let asking = tokio::task::spawn_local({
            let client = paddock.client.clone();
            let url = format!("http://{}/paddock/leases", paddock.addr);
            async move {
                client
                    .post(url)
                    .bearer_auth("k-mac")
                    .body(r#"{"model":"iq2_xs","hold":"heartbeat"}"#)
                    .send()
                    .await
            }
        });
        until_waiters(&paddock.engine, 1).await;

        // Aborting drops the client's connection, as a hang-up does.
        asking.abort();

        until_waiters(&paddock.engine, 0).await;
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}
