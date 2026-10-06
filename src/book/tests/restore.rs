use super::*;

#[test]
fn a_restored_model_with_a_configured_name_is_that_model() {
    let mut book = book();
    let loaded = vec![found("laya", footprint(&book, "laya"))];

    assert_eq!(book.restore(Moment(1_000), loaded, &[], vec![]), []);
    assert_eq!(
        model_view(&book, 1_000, "laya").map(|v| v.unknown),
        Some(false)
    );
    assert_eq!(
        ask(&mut book, 1_001, 1, "laya", Priority::Interactive),
        [forward(1, "laya")]
    );
}

#[test]
fn snapshot_reports_models_leases_waiters_and_errors() {
    let mut book = book();
    let _ = ask(&mut book, 1, 1, "iq2_xs", Priority::Interactive);
    let failed = |error: &str| Event::LoadFailed {
        model: m("iq2_xs"),
        error: error.to_owned(),
    };
    let _ = book.handle(Moment(2), failed("first"));
    let _ = book.handle(Moment(3), failed("second"));
    warm(&mut book, 5, "laya");
    let _ = take(&mut book, 10, 2, 11, "laya", None);
    let _ = ask(&mut book, 20, 3, QWEN, Priority::Interactive);
    assert_eq!(
        ask(&mut book, 25, 4, "laya", Priority::Interactive),
        [forward(4, "laya")]
    );

    let snapshot = book.snapshot(Moment(30));
    assert_eq!(snapshot.models.len(), 5);
    // A request in flight is use now.
    assert_eq!(
        model_view(&book, 30, "laya"),
        Some(ModelView {
            name: m("laya"),
            state: State::Loaded,
            in_flight: 1,
            last_used: Moment(30),
            held_by: vec![ClientName::from("bench-01")],
            unknown: false,
            stray: false,
            placement: None,
            footprint: footprint(&book, "laya"),
        })
    );
    assert_eq!(
        model_view(&book, 30, QWEN),
        Some(ModelView {
            name: m(QWEN),
            state: State::Loading,
            in_flight: 0,
            last_used: Moment(0),
            held_by: vec![],
            unknown: false,
            stray: false,
            placement: None,
            footprint: footprint(&book, QWEN),
        })
    );
    assert_eq!(snapshot.leases, book.leases());
    assert_eq!(
        snapshot.waiters,
        [WaiterView {
            client: ClientName::from("mac-sessions"),
            model: m(QWEN),
            kind: WaiterKind::Request,
            priority: Priority::Interactive,
            since: Moment(20),
            reason: Some(loading(QWEN)),
            estimate: Some(Moment(60_020)),
        }]
    );
    assert_eq!(
        snapshot.errors,
        [LoadError {
            model: m("iq2_xs"),
            at: Moment(3),
            error: "second".to_owned(),
        }]
    );
}

#[test]
fn a_waiting_lease_shows_as_a_lease_waiter() {
    let mut book = book();
    let _ = take(&mut book, 10, 1, 11, "laya", None);

    let waiters = book.snapshot(Moment(10)).waiters;
    assert_eq!(waiters.len(), 1);
    assert_eq!(waiters[0].kind, WaiterKind::Lease);
    assert_eq!(waiters[0].client, ClientName::from("bench-01"));
    assert_eq!(waiters[0].priority, Priority::Batch);
}

#[test]
fn a_restored_lease_whose_id_is_live_is_skipped() {
    let mut book = book();
    assert_eq!(book.max_lease_id(), None);
    let leases = vec![
        restored(lease_ask(7, "laya"), 0),
        restored(lease_ask(7, QWEN), 5),
        restored(lease_ask(3, "laya"), 1),
    ];
    let loaded = vec![found("laya", footprint(&book, "laya"))];

    assert_eq!(book.restore(Moment(1_000), loaded, &[], leases), []);
    let kept = book.lease(LeaseId(7));
    assert_eq!(
        kept.as_ref().map(|lease| lease.model.clone()),
        Some(m("laya"))
    );
    assert_eq!(kept.map(|lease| lease.since), Some(Moment(0)));
    assert_eq!(book.leases().len(), 2);
    assert_eq!(book.max_lease_id(), Some(LeaseId(7)));
}

#[test]
fn a_restored_lease_with_an_id_granted_before_the_restore_is_skipped() {
    let mut book = book();
    let _ = take(&mut book, 10, 1, 7, "laya", None);
    let actions = book.handle(Moment(20), Event::Loaded { model: m("laya") });
    assert_eq!(actions, vec![grant(1, 7), Action::Persist]);

    let leases = vec![restored(lease_ask(7, QWEN), 5)];
    assert_eq!(book.restore(Moment(1_000), vec![], &[], leases), []);

    let kept = book.lease(LeaseId(7)).unwrap();
    assert_eq!(kept.model, m("laya"));
    assert_eq!(kept.since, Moment(20));
    assert_eq!(book.leases().len(), 1);
    assert_eq!(book.state(&m(QWEN)), Some(State::Unloaded));
    assert_eq!(broken(&book), None);
}

#[test]
fn a_restored_lease_on_a_model_not_loaded_loads_it() {
    let mut book = book();
    let leases = vec![restored(lease_ask(7, "laya"), 0)];

    assert_eq!(
        book.restore(Moment(1_000), vec![], &[], leases),
        [Action::Load(m("laya"))]
    );
    assert_eq!(
        book.handle(Moment(2_000), Event::Loaded { model: m("laya") }),
        []
    );
    assert!(book.lease(LeaseId(7)).is_some());
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
}

#[test]
fn a_restored_lease_whose_model_fails_to_load_twice_is_left() {
    let mut book = book();
    let leases = vec![restored(heartbeat(7, "laya", 7_200), 0)];
    let failed = || Event::LoadFailed {
        model: m("laya"),
        error: "no".to_owned(),
    };

    assert_eq!(
        book.restore(Moment(1_000), vec![], &[], leases),
        [Action::Load(m("laya"))]
    );
    assert_eq!(
        book.handle(Moment(2_000), failed()),
        [Action::Load(m("laya"))]
    );
    assert_eq!(book.handle(Moment(3_000), failed()), []);
    assert_eq!(tick(&mut book, 3_600_000), []);
    assert_eq!(book.state(&m("laya")), Some(State::Unloaded));
    assert!(book.lease(LeaseId(7)).is_some());
}

#[test]
fn a_restored_lease_on_a_removed_model_keeps_it_until_it_ends() {
    let toml = test_support::HOST_AND_MODELS.replace(QWEN_SECTION, "");
    let mut book = book_from(&toml);
    let qwen = Footprint {
        vram: Vram::Bytes(22_323 * MIB),
        ram: 4 * GIB,
    };
    let leases = vec![restored(lease_ask(7, QWEN), 0)];

    assert_eq!(
        book.restore(Moment(1_000), vec![found(QWEN, qwen)], &[], leases),
        []
    );
    let view = model_view(&book, 1_000, QWEN);
    assert_eq!(view.as_ref().map(|v| v.unknown), Some(false));
    assert_eq!(
        view.map(|v| v.held_by),
        Some(vec![ClientName::from("bench-01")])
    );
    let attach = Event::HolderAttached { lease: LeaseId(7) };
    assert_eq!(book.handle(Moment(2_000), attach), []);
    assert_eq!(
        ask(&mut book, 3_000, 1, "iq2_xs", Priority::Interactive),
        [refuse(
            1,
            Reason::Held {
                model: m(QWEN),
                client: ClientName::from("bench-01"),
                lease: LeaseId(7),
                since: Moment(0),
                until: None,
                // Nothing saved its activity, so its idle clock starts at the restart.
                idle_since: Some(Moment(1_000)),
            }
        )]
    );
    assert_eq!(
        book.handle(Moment(4_000), Event::LeaseReleased { lease: LeaseId(7) }),
        [
            Action::Unload(m(QWEN)),
            ended(7, Ended::Released),
            Action::Persist
        ]
    );
}

#[test]
fn a_restored_lease_on_a_removed_model_not_loaded_loads_nothing() {
    let toml = test_support::HOST_AND_MODELS.replace(QWEN_SECTION, "");
    let mut book = book_from(&toml);
    let leases = vec![restored(lease_ask(7, QWEN), 0)];

    assert_eq!(book.restore(Moment(1_000), vec![], &[], leases), []);
    assert_eq!(book.state(&m(QWEN)), None);
    assert!(book.lease(LeaseId(7)).is_some());
}

#[test]
fn a_sheep_stand_in_excludes_every_model_on_its_sheep() {
    let mut book = book();
    let stand_in = crate::discover::stand_in(&book.config, "laya").expect("laya is a sheep");
    let loaded = vec![found(stand_in.name.as_str(), stand_in.footprint)];
    assert_eq!(book.restore(Moment(1_000), loaded, &[stand_in], vec![]), []);

    let actions = ask(&mut book, 2_000, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("sheep:laya")), waiting(1, loading("laya"))]
    );
}

#[test]
fn an_ollama_stand_in_excludes_the_configured_model_it_is() {
    let mut book = book();
    let mut stand_in = book.config.models[&m(QWEN)].clone();
    stand_in.name = m("ollama:qwen3.8:27b-ctx131072");
    stand_in.footprint = Footprint {
        vram: Vram::Bytes(GIB),
        ram: GIB,
    };
    let loaded = vec![found(stand_in.name.as_str(), stand_in.footprint)];
    assert_eq!(book.restore(Moment(1_000), loaded, &[stand_in], vec![]), []);

    let actions = ask(&mut book, 2_000, 1, QWEN, Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("ollama:qwen3.8:27b-ctx131072")),
            waiting(1, loading(QWEN)),
        ]
    );
}
