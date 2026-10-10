//! Bare leases over a real loopback socket, so on real time; every await is bounded by `LIMIT`.

use super::*;

/// Reads `response` up to its grant.
async fn granted_on(response: &mut Lines) {
    loop {
        let line = response.next_line().await.expect("a line");
        if line.get("granted").is_some() {
            return;
        }
        assert!(line.get("queued").is_some(), "{line}");
    }
}

#[tokio::test]
async fn a_bare_lease_is_granted_and_names_no_model() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let body = r#"{"footprint":{"vram":"8G","ram":"2G"},"pid":4321}"#;
        let mut response = Lines::from(paddock.take("k-bench", body).await);
        assert_eq!(response.status(), 200);
        granted_on(&mut response).await;

        let leases = paddock.engine.snapshot().await.leases;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].model, None);
        assert_eq!(leases[0].pid, Some(4_321), "the test's socket is loopback");
    })
    .await;
}

#[tokio::test]
async fn a_model_lease_behind_a_bare_lease_is_told_its_name() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        let mut bare = Lines::from(
            paddock
                .take("k-bench", r#"{"footprint":{"vram":"8G","ram":"2G"}}"#)
                .await,
        );
        granted_on(&mut bare).await;

        let mut waiting = Lines::from(paddock.take("k-mac", r#"{"model":"iq2_xs"}"#).await);
        let queued = waiting.next_line().await.expect("a queued line");
        let reason = queued["queued"]["reason"].as_str().expect("a reason");
        assert!(
            reason.starts_with("lease L1 of bench-01 (8G VRAM, 2G RAM) is held by bench-01 since "),
            "{reason}"
        );
        assert_eq!(queued["queued"]["reason_kind"], "held");
    })
    .await;
}

#[tokio::test]
async fn a_take_naming_both_or_neither_or_a_bad_footprint_is_400() {
    with_paddock(FakeShepherd::new(), |paddock| async move {
        for (body, error) in [
            (
                r#"{"model":"iq2_xs","footprint":{"vram":"8G"}}"#,
                "bad_lease_request",
            ),
            (r#"{"priority":"batch"}"#, "bad_lease_request"),
            (r#"{"footprint":{}}"#, "bad_lease_request"),
            (r#"{"footprint":{"vram":"8 GB"}}"#, "bad_lease_request"),
            (r#"{"footprint":{"ram":"all"}}"#, "bad_lease_request"),
            (
                r#"{"footprint":{"vram":"8G","swap":"1G"}}"#,
                "bad_lease_request",
            ),
            (
                r#"{"footprint":{"vram":"8G"},"reclaimable":false}"#,
                "bad_lease_request",
            ),
            (
                r#"{"footprint":{"vram":"8G"},"release_if_idle":"30m"}"#,
                "bad_lease_request",
            ),
            (r#"{"footprint":{"vram":"30G"}}"#, "never_fits"),
        ] {
            let (status, answer) = json_of(paddock.take("k-bench", body).await).await;
            assert_eq!(
                (status, answer["error"].clone()),
                (400, json!(error)),
                "{body}"
            );
        }
        assert!(paddock.engine.snapshot().await.leases.is_empty());
    })
    .await;
}
