//! Revoke through the engine, on a paused clock.

use super::*;
use crate::{
    book::{Ended, Leased, Revocation},
    config::ClientName,
    footprint::{Footprint, Vram},
};

fn bare_on(vram_gib: u64) -> LeaseRequest {
    LeaseRequest {
        leased: Leased::Bare {
            footprint: Footprint {
                vram: Vram::Bytes(vram_gib << 30),
                ram: 1 << 30,
            },
            pid: None,
        },
        ..lease_on("laya", Hold::Connection)
    }
}

fn by_mac(note: Option<&str>) -> Revocation {
    Revocation {
        by: ClientName::from(MAC),
        note: note.map(str::to_owned),
    }
}

#[tokio::test(start_paused = true)]
async fn a_revoked_bare_holder_keeps_its_stream_and_memory_until_it_hangs_up() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = engine.take_lease(BENCH.into(), bare_on(20)).await;
            let lease = granted(&mut events).await;

            let revoked = engine
                .revoke(MAC.into(), lease, Some("forgotten".to_owned()))
                .await;
            assert_eq!(revoked, Ok(()));
            let heard = timeout(BOUND, events.recv()).await.expect("in time");
            assert_eq!(heard, Some(LeaseEvent::Revoked(by_mac(Some("forgotten")))));
            assert!(
                timeout(SOON, events.recv()).await.is_err(),
                "the stream stays open while the job may run"
            );
            assert_eq!(
                engine.snapshot().await.declared.ram,
                1 << 30,
                "still counted"
            );

            drop(events);
            until("the memory freed", || async {
                engine.snapshot().await.declared.ram == 0
            })
            .await;
            assert!(engine.snapshot().await.leases.is_empty());
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_revoked_heartbeat_bare_lease_ends_its_stream_and_frees_its_memory() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let heartbeat = Hold::Heartbeat {
                ttl: Duration::from_secs(60),
            };
            let ask = LeaseRequest {
                hold: heartbeat,
                ..bare_on(20)
            };
            let mut events = engine.take_lease(BENCH.into(), ask).await;
            let lease = granted(&mut events).await;

            assert_eq!(engine.revoke(MAC.into(), lease, None).await, Ok(()));
            assert_eq!(
                timeout(BOUND, events.recv()).await.expect("in time"),
                Some(LeaseEvent::Ended(Ended::Revoked(by_mac(None))))
            );
            assert_eq!(
                timeout(BOUND, events.recv()).await.expect("in time"),
                None,
                "the stream ends"
            );
            assert_eq!(
                engine.snapshot().await.declared.ram,
                0,
                "freed at the revoke"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_revoked_model_lease_ends_its_stream_and_its_model_stays_loaded() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;

            assert_eq!(engine.revoke(MAC.into(), lease, None).await, Ok(()));
            assert_eq!(
                timeout(BOUND, events.recv()).await.expect("in time"),
                Some(LeaseEvent::Ended(Ended::Revoked(by_mac(None))))
            );
            assert_eq!(
                timeout(BOUND, events.recv()).await.expect("in time"),
                None,
                "the stream ends"
            );
            assert_eq!(state_of(&engine, "laya").await, Some(State::Loaded));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn revoking_a_lease_that_has_ended_or_never_was_is_not_found() {
    with_engine(
        config(SHEEP_MODELS),
        FakeShepherd::new(),
        |engine| async move {
            let mut events = engine
                .take_lease(BENCH.into(), lease_on("laya", Hold::Connection))
                .await;
            let lease = granted(&mut events).await;
            assert_eq!(engine.release(BENCH.into(), lease).await, Ok(()));

            assert_eq!(
                engine.revoke(MAC.into(), lease, None).await,
                Err(LeaseRefused::NotFound)
            );
            assert_eq!(
                engine.revoke(MAC.into(), LeaseId(99), None).await,
                Err(LeaseRefused::NotFound)
            );
        },
    )
    .await;
}
