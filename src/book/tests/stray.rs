use super::*;
use crate::config::Backend;

fn backend_of(book: &Book, model: &str) -> Backend {
    book.config.models[&m(model)].backend.clone()
}

fn stray(
    book: &mut Book,
    now: u64,
    model: &str,
    footprint: Footprint,
    backend: Backend,
) -> Vec<Action> {
    book.handle(
        Moment(now),
        Event::StrayFound {
            model: m(model),
            footprint,
            backend,
        },
    )
}

fn stray_laya(book: &mut Book, now: u64) -> Vec<Action> {
    let (footprint, backend) = (footprint(book, "laya"), backend_of(book, "laya"));
    stray(book, now, "laya", footprint, backend)
}

/// A sheep serving both iq2_xs models, found at the larger one's figures.
fn stray_iq2_xs_sheep(book: &mut Book, now: u64) -> Vec<Action> {
    let footprint = Footprint {
        vram: Vram::All,
        ram: 44 * GIB,
    };
    let backend = backend_of(book, "iq2_xs");
    stray(book, now, "sheep:iq2_xs", footprint, backend)
}

fn reclaimable(lease: u64, model: &str) -> LeaseAsk {
    LeaseAsk {
        reclaimable: true,
        ..lease_ask(lease, model)
    }
}

#[test]
fn a_stray_of_a_configured_model_is_that_model_loaded() {
    let mut book = book();
    assert_eq!(stray_laya(&mut book, 0), vec![]);
    let view = model_view(&book, 0, "laya").expect("laya");
    assert_eq!(
        (view.state, view.stray, view.unknown),
        (State::Loaded, true, false)
    );
    assert_eq!(
        ask(&mut book, 10, 1, "laya", Priority::Interactive),
        vec![forward(1, "laya")]
    );
}

#[test]
fn a_waiter_for_a_configured_model_is_served_by_its_stray() {
    let mut book = book();
    let _ = take(&mut book, 0, 1, 1, "iq3_s", None);
    let _ = book.handle(Moment(0), Event::Loaded { model: m("iq3_s") });
    let waits = ask_lease(&mut book, 10, 2, lease_ask(2, "laya"));
    assert!(
        matches!(waits.as_slice(), [Action::Waiting { .. }]),
        "iq3_s is held and excludes laya: {waits:?}"
    );
    assert_eq!(
        stray_laya(&mut book, 20),
        vec![grant(2, 2), Action::Persist]
    );
}

#[test]
fn a_stray_is_reclaimable() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    let actions = ask(&mut book, 10, 1, "iq3_s", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, loading("iq3_s"))]
    );
}

#[test]
fn a_stand_in_stray_is_unknown_and_excludes_the_models_on_its_sheep() {
    let mut book = book();
    assert_eq!(stray_iq2_xs_sheep(&mut book, 0), vec![]);
    let view = model_view(&book, 0, "sheep:iq2_xs").expect("the stand-in");
    assert_eq!(
        (view.state, view.stray, view.unknown),
        (State::Loaded, true, true)
    );

    let actions = ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Unload(m("sheep:iq2_xs")),
            waiting(1, loading("iq2_xs"))
        ]
    );
}

#[test]
fn a_stray_a_lease_names_is_not_unknown() {
    let mut book = book_from(&test_support::HOST_AND_MODELS.replace(QWEN_SECTION, ""));
    let lease = restored(reclaimable(1, QWEN), 0);
    let _ = book.restore(Moment(0), vec![], &[], vec![lease]);
    let qwen = book_from(test_support::HOST_AND_MODELS);
    let (footprint, backend) = (footprint(&qwen, QWEN), backend_of(&qwen, QWEN));
    let _ = stray(&mut book, 10, QWEN, footprint, backend);
    let view = model_view(&book, 10, QWEN).expect("qwen");
    assert_eq!((view.stray, view.unknown), (true, false));
}

#[test]
fn a_stray_that_exits_by_itself_is_forgotten_and_not_stopped() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    assert_eq!(
        book.handle(Moment(10), Event::BackendExited { model: m("laya") }),
        vec![]
    );
    let view = model_view(&book, 10, "laya").expect("laya is configured");
    assert_eq!((view.state, view.stray), (State::Unloaded, false));

    let _ = stray_iq2_xs_sheep(&mut book, 20);
    assert_eq!(
        book.handle(
            Moment(30),
            Event::BackendExited {
                model: m("sheep:iq2_xs")
            }
        ),
        vec![]
    );
    assert_eq!(
        book.state(&m("sheep:iq2_xs")),
        None,
        "a stand-in is forgotten whole"
    );
}

#[test]
fn a_stray_being_evicted_that_exits_frees_its_room_at_once() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    let _ = ask(&mut book, 5, 1, "laya", Priority::Interactive);
    let _ = ask(&mut book, 10, 2, "iq3_s", Priority::Interactive);
    assert_eq!(book.state(&m("laya")), Some(State::Evicting));
    assert_eq!(
        book.handle(Moment(20), Event::BackendExited { model: m("laya") }),
        vec![
            Action::Load(m("iq3_s")),
            waiting_until(2, loading("iq3_s"), 60_020)
        ]
    );
    assert!(!model_view(&book, 20, "laya").expect("laya").stray);
}

#[test]
fn a_reclaimable_lease_on_a_stray_ends_reclaimed_when_it_exits() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    assert_eq!(
        ask_lease(&mut book, 10, 1, reclaimable(1, "laya")),
        vec![grant(1, 1), Action::Persist]
    );
    assert_eq!(
        book.handle(Moment(20), Event::BackendExited { model: m("laya") }),
        vec![ended(1, Ended::Reclaimed), Action::Persist]
    );
    assert_eq!(book.leases(), vec![]);
}

#[test]
fn a_reclaimable_lease_on_a_stray_ends_reclaimed_when_it_is_evicted() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    let _ = ask_lease(&mut book, 10, 1, reclaimable(1, "laya"));
    assert_eq!(
        ask(&mut book, 20, 2, "iq3_s", Priority::Interactive),
        vec![
            Action::Unload(m("laya")),
            waiting(2, loading("iq3_s")),
            ended(1, Ended::Reclaimed),
            Action::Persist
        ]
    );
}

#[test]
fn a_stray_for_a_model_the_dog_is_loading_changes_nothing() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    assert_eq!(stray_laya(&mut book, 10), vec![]);
    let view = model_view(&book, 10, "laya").expect("laya");
    assert_eq!((view.state, view.stray), (State::Loading, false));
}

#[test]
fn a_stray_for_a_model_the_dog_has_loaded_is_not_a_stray() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    assert_eq!(stray_laya(&mut book, 10), vec![]);
    let view = model_view(&book, 10, "laya").expect("laya");
    assert_eq!((view.state, view.stray), (State::Loaded, false));
}

#[test]
fn a_stray_is_no_longer_marked_once_it_unloads() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    let _ = ask(&mut book, 10, 1, "iq3_s", Priority::Interactive);
    let _ = book.handle(Moment(20), Event::Unloaded { model: m("laya") });
    assert!(!model_view(&book, 30, "laya").expect("laya").stray);
}

#[test]
fn restore_marks_a_found_stray() {
    let mut book = book();
    let laya = Found {
        stray: true,
        ..found("laya", footprint(&book, "laya"))
    };
    let _ = book.restore(Moment(1_000), vec![laya], &[], vec![]);
    assert!(model_view(&book, 1_000, "laya").expect("laya").stray);
}

#[test]
fn a_small_stand_in_stray_still_excludes_the_models_on_its_sheep() {
    let mut book = book();
    let footprint = Footprint {
        vram: Vram::None,
        ram: GIB,
    };
    let backend = backend_of(&book, "iq2_xs");
    let _ = stray(&mut book, 0, "sheep:iq2_xs", footprint, backend);
    assert_eq!(
        ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive),
        vec![
            Action::Unload(m("sheep:iq2_xs")),
            waiting(1, loading("iq2_xs"))
        ],
        "iq2_xs would fit beside it, but not on the sheep it runs on"
    );
}

#[test]
fn a_stray_counts_as_used_when_it_is_found() {
    let mut book = book();
    let _ = stray_laya(&mut book, 1_000_000);
    assert_eq!(
        ask(&mut book, 1_000_010, 1, "iq3_s", Priority::Batch),
        vec![Action::Refuse {
            waiter: WaiterId(1),
            refusal: Refusal {
                reason: Reason::Grace {
                    model: m("laya"),
                    until: Moment(1_120_000)
                },
                retry_after: Some(Duration::from_millis(179_990)),
            },
        }],
        "a batch waiter may not evict it until the grace period from its finding ends"
    );
}

/// The stray left while evicted for iq3_s, so a later crash of laya is not that eviction.
#[test]
fn a_stray_that_exits_while_evicted_stops_naming_what_it_was_evicted_for() {
    let mut book = book();
    let _ = stray_laya(&mut book, 0);
    let _ = ask(&mut book, 5, 1, "laya", Priority::Interactive);
    let _ = ask(&mut book, 10, 2, "iq3_s", Priority::Interactive);
    let _ = book.handle(Moment(20), Event::BackendExited { model: m("laya") });
    let _ = book.handle(Moment(30), finished("laya"));
    let _ = book.handle(Moment(30), Event::Loaded { model: m("iq3_s") });
    let _ = book.handle(Moment(30), finished("iq3_s"));

    let _ = ask(&mut book, 40, 3, "laya", Priority::Interactive);
    let _ = book.handle(Moment(50), Event::Unloaded { model: m("iq3_s") });
    let _ = book.handle(Moment(60), Event::Loaded { model: m("laya") });
    let _ = book.handle(Moment(70), Event::BackendExited { model: m("laya") });
    assert_eq!(
        ask(&mut book, 80, 4, "laya", Priority::Interactive),
        vec![waiting(4, Reason::Draining { model: m("laya") })]
    );
}

#[test]
fn a_stray_counts_at_what_it_was_found_with_when_that_is_more() {
    let mut book = book();
    let found = Footprint {
        vram: Vram::None,
        ram: 7 * GIB,
    };
    let backend = backend_of(&book, "laya");
    let _ = stray(&mut book, 0, "laya", found, backend);
    assert_eq!(
        model_view(&book, 0, "laya").expect("laya").footprint.ram,
        7 * GIB
    );
}

#[test]
fn a_stray_found_while_a_model_is_reserved_does_not_strand_its_claim() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = ask_lease(&mut book, 200_000, 1, lease_ask(1, "iq3_s"));
    assert_eq!(book.state(&m("iq3_s")), Some(State::Reserved));
    assert_eq!(
        stray_iq2_xs_sheep(&mut book, 200_010),
        vec![waiting_until(
            1,
            Reason::Grace {
                model: m("sheep:iq2_xs"),
                until: Moment(320_010)
            },
            380_010
        )],
        "the claim is made again, and the stray is what blocks it now"
    );
    assert_eq!(
        book.handle(Moment(200_020), Event::Unloaded { model: m("laya") }),
        vec![]
    );
    assert_eq!(book.next_deadline(), Some(Moment(320_010)));
    assert_eq!(
        tick(&mut book, 320_010),
        vec![
            Action::Unload(m("sheep:iq2_xs")),
            waiting(1, loading("iq3_s"))
        ]
    );
    assert_eq!(
        book.handle(
            Moment(320_020),
            Event::Unloaded {
                model: m("sheep:iq2_xs")
            }
        ),
        vec![
            Action::Load(m("iq3_s")),
            waiting_until(1, loading("iq3_s"), 380_020)
        ]
    );
}
