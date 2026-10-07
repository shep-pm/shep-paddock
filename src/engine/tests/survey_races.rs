//! The races a survey reading loses to the dog's own jobs and process events: `touched`, the
//! linger after an ollama unload, and the guards that ignored an `online`. Paused clock.

use shep_client::shep_core::status::ProcStatus;

use super::{
    survey::with_ollama,
    survey_strays::{BASE, flock_of, idle, listed_nothing, listing, loaded_by_the_dog, row},
    *,
};

/// The flock was read while laya still ran, and is applied after its exit.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_a_stray_sheep_exits_does_not_bring_it_back() {
    let mut engine = engine();
    let laya = ModelName::from("laya");
    engine.process(online("laya"), idle);
    sleep(SOON).await;
    let asked = Instant::now();
    sleep(SOON).await;
    engine.process(crash("laya", ProcessKind::Exit, false), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Unloaded));

    let _ = engine.surveyed(flock_of(asked, vec![row("laya", ProcStatus::Online)]), idle);
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Unloaded),
        "the reading was older than the exit"
    );

    sleep(SOON).await;
    let lines = engine.surveyed(
        flock_of(Instant::now(), vec![row("laya", ProcStatus::Online)]),
        idle,
    );
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Loaded),
        "a reading taken after it is a stray"
    );
    assert_eq!(
        lines,
        vec!["paddock: sheep laya is running without the dog; counting it as laya".to_owned()]
    );
}

/// The survey asked /api/ps while qwen was still loaded, and its answer arrives after the
/// dog unloaded qwen for sitting idle.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_an_unload_does_not_bring_the_model_back() {
    let mut engine = loaded_by_the_dog(with_ollama(BASE), QWEN);
    let qwen = ModelName::from(QWEN);
    let asked = Instant::now();
    sleep(Duration::from_secs(2 * 3_600)).await;
    engine.feed(Event::Tick);
    assert_eq!(engine.book.state(&qwen), Some(State::Unloading));
    let _ = engine.take_jobs();
    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloading),
        "the dog's own unload is not a stray"
    );
    engine.finished(qwen.clone(), Outcome::Unloaded { sheep: None });
    let unloaded_at = Instant::now();

    let _ = engine.surveyed(listing(asked), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloaded),
        "the listing was older than the unload"
    );
    let _ = engine.surveyed(listing(unloaded_at), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Unloaded),
        "nor is one asked at the unload's instant trusted"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(listed_nothing(Instant::now()), idle);
    sleep(SOON).await;
    let lines = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Loaded),
        "once seen gone, a listing taken after it is a stray"
    );
    assert_eq!(
        lines,
        vec![
            "paddock: ollama gpu-ollama has qwen3.8:27b-ctx65536 loaded without the dog; \
             counting it as qwen3.8:27b"
                .to_owned()
        ]
    );
}

/// qwen loaded by the dog, then unloaded for sitting idle, as its unload reports.
async fn unloaded_by_the_dog() -> Engine {
    let mut engine = loaded_by_the_dog(with_ollama(BASE), QWEN);
    sleep(Duration::from_secs(2 * 3_600)).await;
    engine.feed(Event::Tick);
    let _ = engine.take_jobs();
    engine.finished(QWEN.into(), Outcome::Unloaded { sheep: None });
    engine
}

/// ollama answers `keep_alive: 0` before the runner is gone, and lists the model meanwhile.
#[tokio::test(start_paused = true)]
async fn a_model_ollama_lists_just_after_the_dogs_unload_is_not_a_stray_until_seen_gone() {
    let mut engine = unloaded_by_the_dog().await;
    let qwen = ModelName::from(QWEN);
    sleep(SOON).await;

    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(engine.book.state(&qwen), Some(State::Unloaded));

    sleep(SOON).await;
    let _ = engine.surveyed(listed_nothing(Instant::now()), idle);
    sleep(SOON).await;
    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(
        engine.book.state(&qwen),
        Some(State::Loaded),
        "loaded again after it was gone"
    );
}

#[tokio::test(start_paused = true)]
async fn a_model_ollama_still_lists_two_minutes_after_the_dogs_unload_is_a_stray() {
    let mut engine = unloaded_by_the_dog().await;
    let qwen = ModelName::from(QWEN);

    sleep(Duration::from_secs(119)).await;
    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(engine.book.state(&qwen), Some(State::Unloaded));

    sleep(SOON).await;
    let _ = engine.surveyed(listing(Instant::now()), idle);
    assert_eq!(engine.book.state(&qwen), Some(State::Loaded));
}

/// A hand start races the dog's idle stop of iq2_xs, and outlives it.
#[tokio::test(start_paused = true)]
async fn an_online_ignored_while_the_dog_stopped_its_sheep_is_found_by_a_later_survey() {
    let mut engine = loaded_by_the_dog(config(SHEEP_MODELS), "iq2_xs");
    let iq2_xs = ModelName::from("iq2_xs");
    let stand_in = ModelName::from("sheep:iq2_xs");
    sleep(Duration::from_secs(2 * 3_600)).await;
    engine.feed(Event::Tick);
    assert_eq!(engine.book.state(&iq2_xs), Some(State::Unloading));
    let _ = engine.take_jobs();
    engine.process(online("iq2_xs"), |_| true);
    let asked = Instant::now();
    sleep(SOON).await;
    let running = || vec![row("iq2_xs", ProcStatus::Online)];
    let _ = engine.surveyed(flock_of(Instant::now(), running()), |_| true);
    assert_eq!(
        engine.book.state(&stand_in),
        None,
        "the stop is still running"
    );
    engine.finished(
        iq2_xs.clone(),
        Outcome::Unloaded {
            sheep: Some("iq2_xs".into()),
        },
    );
    assert_eq!(engine.book.state(&iq2_xs), Some(State::Unloaded));

    let _ = engine.surveyed(flock_of(asked, running()), idle);
    assert_eq!(
        engine.book.state(&stand_in),
        None,
        "read before the stop ended"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(flock_of(Instant::now(), running()), idle);
    assert_eq!(engine.book.state(&stand_in), Some(State::Loaded));
}

/// shep held laya stopped after its crash, so the dog's stop of it publishes no `Stop` and
/// its mark stays. The hand start's `online` is not counted, and clears the mark.
#[tokio::test(start_paused = true)]
async fn a_hand_start_a_stale_stop_mark_hid_is_found_by_the_next_survey() {
    let mut engine = loaded_by_the_dog(config(SHEEP_MODELS), "laya");
    let laya = ModelName::from("laya");
    engine.process(crash("laya", ProcessKind::Exit, false), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Unloading));
    let _ = engine.take_jobs();
    engine.finished(
        laya.clone(),
        Outcome::Unloaded {
            sheep: Some("laya".into()),
        },
    );
    sleep(SOON).await;

    engine.process(online("laya"), idle);
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Unloaded),
        "the mark hid it"
    );

    sleep(SOON).await;
    let _ = engine.surveyed(
        flock_of(Instant::now(), vec![row("laya", ProcStatus::Online)]),
        idle,
    );
    assert_eq!(engine.book.state(&laya), Some(State::Loaded));
    assert!(engine.snapshot().models.iter().any(|view| view.stray));
}

fn ask_for(engine: &mut Engine, model: &str) {
    engine.feed(Event::RequestArrived {
        waiter: WaiterId(1),
        client: MAC.into(),
        model: model.into(),
        priority: Priority::Interactive,
        max_wait: MAX_WAIT,
    });
    let _ = engine.take_jobs();
}

/// laya's retried restart comes online before the reading is asked, so only the failed
/// load's outcome says the reading is stale.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_a_load_fails_for_good_does_not_count_its_sheep() {
    let mut engine = engine();
    let laya = ModelName::from("laya");
    ask_for(&mut engine, "laya");
    engine.finished(laya.clone(), Outcome::LoadFailed("not ready".to_owned()));
    assert_eq!(engine.book.state(&laya), Some(State::Loading), "the retry");
    let _ = engine.take_jobs();
    engine.process(online("laya"), |_| true);
    sleep(SOON).await;
    let asked = Instant::now();
    sleep(SOON).await;
    engine.finished(laya.clone(), Outcome::LoadFailed("not ready".to_owned()));
    assert_eq!(engine.book.state(&laya), Some(State::Unloaded));

    let running = || vec![row("laya", ProcStatus::Online)];
    let _ = engine.surveyed(flock_of(asked, running()), idle);
    assert_eq!(engine.book.state(&laya), Some(State::Unloaded));

    sleep(SOON).await;
    let _ = engine.surveyed(flock_of(Instant::now(), running()), idle);
    assert_eq!(
        engine.book.state(&laya),
        Some(State::Loaded),
        "it runs on after the failure"
    );
}

/// The book gives up on qwen's load while its job runs, as it does on a backend that exits
/// twice, and the job then reports Loaded: the engine stops it quietly.
#[tokio::test(start_paused = true)]
async fn a_survey_from_before_a_load_the_book_gave_up_on_does_not_count_it() {
    let (notify, _) = mpsc::unbounded_channel();
    let mut engine = Engine::new(with_ollama(BASE), Clock::new(), notify);
    let qwen = ModelName::from(QWEN);
    ask_for(&mut engine, QWEN);
    for _ in 0..2 {
        engine.feed(Event::BackendExited {
            model: qwen.clone(),
        });
        let _ = engine.take_jobs();
    }
    assert_eq!(engine.book.state(&qwen), Some(State::Unloaded));
    let asked = Instant::now();
    sleep(SOON).await;
    engine.finished(qwen.clone(), Outcome::Loaded);
    assert_eq!(engine.take_jobs().len(), 1, "the quiet stop");

    let _ = engine.surveyed(listing(asked), idle);
    assert_eq!(engine.book.state(&qwen), Some(State::Unloaded));
}
