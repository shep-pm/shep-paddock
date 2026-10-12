//! A loaded model whose launch settings changed is stale, and reloads once nothing would be cut off.

use super::*;
use crate::config::Backend;

/// A book on `base` with `model` loaded and idle at moment 0, then reconfigured at moment 10 to
/// `base` with `from` replaced by `to`.
fn reconfigured(base: &str, model: &str, from: &str, to: &str) -> Book {
    assert!(base.contains(from), "{from} is not in the config");
    let mut book = book_from(base);
    warm(&mut book, 0, model);
    let _ = book.reconfigure(Moment(10), test_support::config(&base.replace(from, to)));
    book
}

fn stale(book: &Book, model: &str) -> bool {
    model_view(book, 10, model).is_some_and(|view| view.stale)
}

const IQ2_XS_ENV: &str = r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }"#;
const IQ3_S_BACKEND: &str = r#"backend = { sheep = "iq3_s" }"#;
const LAYA_SCRIPT: &str = "/opt/laya/venv-gpu/bin/laya-serve";

/// iq3_s with turns and a container, for the settings that change its footprint.
fn turns_toml() -> String {
    test_support::HOST_AND_MODELS.replace(
        "excludes = [\"laya\"]\nidle = \"2h\"",
        "excludes = [\"laya\"]\nidle = \"2h\"\nsequences = 2\nturn_ram = \"1G\"\ncontainer = \"strata\"",
    )
}

#[test]
fn a_changed_backend_env_args_or_script_marks_a_loaded_model_stale() {
    let env = reconfigured(
        test_support::HOST_AND_MODELS,
        "iq2_xs",
        IQ2_XS_ENV,
        r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
    );
    assert!(stale(&env, "iq2_xs"));
    assert!(!stale(&env, "iq2_xs-256k"));

    let args = reconfigured(
        test_support::HOST_AND_MODELS,
        "iq3_s",
        IQ3_S_BACKEND,
        r#"backend = { sheep = "iq3_s", args = ["--ctx", "1"] }"#,
    );
    assert!(stale(&args, "iq3_s"));

    let placed = test_support::placed_toml();
    let script = reconfigured(&placed, "laya", LAYA_SCRIPT, "/opt/laya/venv-gpu/bin/other");
    assert!(stale(&script, "laya"));
}

#[test]
fn a_changed_turn_ram_sequences_or_container_marks_it_stale() {
    let base = turns_toml();
    for (from, to) in [
        ("sequences = 2", "sequences = 3"),
        ("turn_ram = \"1G\"", "turn_ram = \"2G\""),
        ("container = \"strata\"", "container = \"other\""),
    ] {
        let book = reconfigured(&base, "iq3_s", from, to);
        assert!(stale(&book, "iq3_s"), "{from} became {to}");
    }
}

#[test]
fn a_changed_env_in_a_placement_marks_stale_only_the_model_running_in_it() {
    let placed = test_support::placed_toml();
    // laya loads in its gpu placement, the first that fits.
    let other = reconfigured(
        &placed,
        "laya",
        r#"env = { LAYA_DEVICE = "cpu", CUDA_VISIBLE_DEVICES = "" }"#,
        r#"env = { LAYA_DEVICE = "cpu", CUDA_VISIBLE_DEVICES = "1" }"#,
    );
    assert_eq!(
        model_view(&other, 10, "laya").map(|v| v.placement),
        Some(Some(PlacementName::from("gpu")))
    );
    assert!(!stale(&other, "laya"));

    let running = reconfigured(
        &placed,
        "laya",
        r#"env = { LAYA_DEVICE = "cuda", CUDA_VISIBLE_DEVICES = "0" }"#,
        r#"env = { LAYA_DEVICE = "cuda", CUDA_VISIBLE_DEVICES = "1" }"#,
    );
    assert!(stale(&running, "laya"));
}

#[test]
fn a_changed_idle_excludes_prefix_apis_or_name_is_not_stale() {
    let base = test_support::HOST_AND_MODELS;
    for (model, from, to) in [
        ("laya", "idle = \"8h\"", "idle = \"4h\""),
        ("iq3_s", "excludes = [\"laya\"]", "excludes = []"),
        ("laya", "prefix = \"/laya\"", "prefix = \"/other\""),
        (
            "laya",
            "prefix = \"/laya\"",
            "prefix = \"/laya\"\napis = [\"openai\"]",
        ),
        (
            "laya",
            "key = \"k-laya\"",
            "key = \"k-laya\"\nname = \"laya-2\"",
        ),
        ("laya", "http://127.0.0.1:8000", "http://127.0.0.1:8001"),
        ("laya", "key = \"k-laya\"", "key = \"k-other\""),
        ("laya", "ram = \"5G\"", "ram = \"6G\""),
        (
            "laya",
            "ready = { path = \"/health\", field = \"loaded\" }\nram",
            "ready = { path = \"/up\" }\nload_timeout = \"1m\"\nram",
        ),
    ] {
        let book = reconfigured(base, model, from, to);
        assert!(!stale(&book, model), "{from} became {to}");
    }
}

#[test]
fn an_ollama_model_is_never_stale() {
    let book = reconfigured(
        test_support::HOST_AND_MODELS,
        QWEN,
        "name = \"qwen3.8:27b-ctx131072\"",
        "name = \"qwen3.8:27b-ctx4096\"",
    );
    assert!(!stale(&book, QWEN));
}

#[test]
fn a_stray_an_unknown_model_and_a_restored_model_are_never_stale() {
    let changed = test_support::config(&test_support::HOST_AND_MODELS.replace(
        IQ2_XS_ENV,
        r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
    ));

    // A stray is not the dog's to restart.
    let mut book = book();
    let backend = book.config.models[&m("iq2_xs")].backend.clone();
    let footprint = footprint(&book, "iq2_xs");
    let _ = book.handle(
        Moment(0),
        Event::StrayFound {
            model: m("iq2_xs"),
            footprint,
            backend,
        },
    );
    let _ = book.reconfigure(Moment(10), Arc::clone(&changed));
    assert!(!stale(&book, "iq2_xs"));

    // An unknown model has no config to differ from.
    let mut book = book_from(test_support::HOST_AND_MODELS);
    let loaded = vec![found("mystery", footprint)];
    let _ = book.restore(Moment(0), loaded, &[], vec![]);
    let _ = book.reconfigure(Moment(10), Arc::clone(&changed));
    assert_eq!(
        model_view(&book, 10, "mystery").map(|v| v.unknown),
        Some(true)
    );
    assert!(!stale(&book, "mystery"));

    // A restored model took the config read at start, so a dog restart forgets staleness.
    let mut book = book_from(test_support::HOST_AND_MODELS);
    let loaded = vec![found("iq2_xs", footprint)];
    let _ = book.restore(Moment(0), loaded, &[], vec![]);
    assert!(!stale(&book, "iq2_xs"));
    assert_eq!(tick(&mut book, 3_600_000), []);

    // A stand-in is a process the dog did not start.
    let mut book = book_from(test_support::HOST_AND_MODELS);
    let mut stand_in = book.config.models[&m("iq2_xs")].clone();
    stand_in.backend = Backend::Sheep {
        sheep: "elsewhere".to_owned(),
        name: None,
        script: None,
        args: None,
        env: Default::default(),
    };
    let loaded = vec![found("iq2_xs", footprint)];
    let _ = book.restore(Moment(0), loaded, &[stand_in], vec![]);
    let _ = book.reconfigure(Moment(10), changed);
    assert!(!stale(&book, "iq2_xs"));
}

const GRACE: u64 = 120_000;

/// [`test_support::HOST_AND_MODELS`] with iq2_xs started with another `CONTEXT`.
fn changed() -> Arc<Config> {
    test_support::config(&test_support::HOST_AND_MODELS.replace(
        IQ2_XS_ENV,
        r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
    ))
}

/// A book with iq2_xs loaded at moment 0, then reconfigured at moment 10 so it is stale.
fn stale_iq2_xs() -> Book {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    assert_eq!(book.reconfigure(Moment(10), changed()), []);
    book
}

fn unload(model: &str) -> Action {
    Action::Unload(m(model))
}

fn load(model: &str) -> Action {
    Action::Load(m(model))
}

fn unloaded(model: &str) -> Event {
    Event::Unloaded { model: m(model) }
}

fn loaded(model: &str) -> Event {
    Event::Loaded { model: m(model) }
}

#[test]
fn a_stale_model_unloads_once_nothing_holds_uses_or_waits_for_it_and_its_grace_has_passed() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");

    assert_eq!(
        book.reconfigure(Moment(5 * GRACE), changed()),
        [unload("iq2_xs")]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Unloading));
}

#[test]
fn a_stale_model_held_by_a_lease_is_not_reloaded() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    let _ = take(&mut book, 10, 1, 11, "iq2_xs", None);

    assert_eq!(book.reconfigure(Moment(5 * GRACE), changed()), []);
    assert_eq!(tick(&mut book, 6 * GRACE), []);
    assert!(stale(&book, "iq2_xs"));
    // Its holder letting go is the moment nothing holds it.
    assert_eq!(
        book.handle(
            Moment(7 * GRACE),
            Event::LeaseReleased { lease: LeaseId(11) }
        ),
        [
            unload("iq2_xs"),
            ended(11, Ended::Released),
            Action::Persist
        ]
    );
}

#[test]
fn a_stale_model_with_a_request_in_flight_is_not_reloaded() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    assert_eq!(
        ask(&mut book, 10, 1, "iq2_xs", Priority::Interactive),
        [forward(1, "iq2_xs")]
    );

    assert_eq!(book.reconfigure(Moment(5 * GRACE), changed()), []);
    assert_eq!(tick(&mut book, 6 * GRACE), []);
    // The request's end is use, so its grace period starts there.
    assert_eq!(book.handle(Moment(7 * GRACE), finished(1, "iq2_xs")), []);
    assert_eq!(book.next_deadline(), Some(Moment(8 * GRACE)));
    assert_eq!(tick(&mut book, 8 * GRACE), [unload("iq2_xs")]);
}

#[test]
fn a_stale_model_with_a_waiter_is_not_reloaded() {
    let mut book = book_from(&turns_toml());
    warm(&mut book, 0, "iq3_s");
    assert_eq!(
        ask(&mut book, 10, 1, "iq3_s", Priority::Interactive),
        [forward(1, "iq3_s")]
    );
    // The second request waits for the request turn the first holds.
    let waiting = ask(&mut book, 20, 2, "iq3_s", Priority::Interactive);
    assert!(matches!(waiting.as_slice(), [Action::Waiting { .. }]));
    let other = test_support::config(&turns_toml().replace("sequences = 2", "sequences = 3"));
    assert_eq!(book.reconfigure(Moment(30_000), other), []);

    // The first request finishing hands its turn to the waiter, which is served before any reload.
    assert_eq!(
        book.handle(Moment(60_000), finished(1, "iq3_s")),
        [forward(2, "iq3_s")]
    );
    assert_eq!(tick(&mut book, 60_000 + 2 * GRACE), []);
    assert_eq!(book.state(&m("iq3_s")), Some(State::Loaded));
    let done = 60_000 + 2 * GRACE;
    assert_eq!(book.handle(Moment(done), finished(2, "iq3_s")), []);
    assert_eq!(tick(&mut book, done + GRACE), [unload("iq3_s")]);
}

#[test]
fn a_stale_model_inside_its_grace_period_is_reloaded_on_the_tick_that_ends_it() {
    let mut book = stale_iq2_xs();

    assert_eq!(book.next_deadline(), Some(Moment(GRACE)));
    assert_eq!(tick(&mut book, GRACE - 1), []);
    assert_eq!(tick(&mut book, GRACE), [unload("iq2_xs")]);
}

#[test]
fn a_stale_model_that_cannot_fit_at_its_new_figures_stays_loaded_and_stale() {
    // iq2_xs started with another CONTEXT, and needing 60G of RAM, which laya's 5G passes.
    let grown_toml = test_support::HOST_AND_MODELS
        .replace("ram = \"37G\"", "ram = \"60G\"")
        .replace(
            IQ2_XS_ENV,
            r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
        );
    let without_laya = || {
        test_support::config(&grown_toml.replace("excludes = [\"laya\"]\n", "").replace(
            r#"[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "k-laya"
ready = { path = "/health", field = "loaded" }
ram = "5G"
idle = "8h"
"#,
            "",
        ))
    };
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    warm(&mut book, 0, "laya");
    let grown = test_support::config(&grown_toml);
    assert_eq!(book.reconfigure(Moment(10), grown), []);

    // 60G beside laya's 5G passes the host's RAM.
    assert_eq!(tick(&mut book, 2 * GRACE), []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
    assert!(stale(&book, "iq2_xs"));

    // Once laya is gone it fits, and the next event starts the reload.
    assert_eq!(
        book.reconfigure(Moment(3 * GRACE), without_laya()),
        [unload("laya")]
    );
    assert_eq!(
        book.handle(Moment(3 * GRACE + 1), unloaded("laya")),
        [unload("iq2_xs")]
    );
}

#[test]
fn a_reload_that_brings_back_the_launch_settings_clears_stale_and_reloads_nothing() {
    let mut book = stale_iq2_xs();
    assert!(stale(&book, "iq2_xs"));

    assert_eq!(
        book.reconfigure(
            Moment(20),
            test_support::config(test_support::HOST_AND_MODELS)
        ),
        []
    );
    assert!(!stale(&book, "iq2_xs"));
    assert_eq!(tick(&mut book, 5 * GRACE), []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
}

#[test]
fn a_reloaded_model_is_loaded_again_with_the_new_settings_and_is_not_stale() {
    let mut book = stale_iq2_xs();
    assert_eq!(tick(&mut book, GRACE), [unload("iq2_xs")]);

    assert_eq!(
        book.handle(Moment(GRACE + 1), unloaded("iq2_xs")),
        [load("iq2_xs")]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loading));
    assert_eq!(book.handle(Moment(GRACE + 2), loaded("iq2_xs")), []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
    assert!(!stale(&book, "iq2_xs"));
    assert_eq!(tick(&mut book, 10 * GRACE), []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loaded));
}

#[test]
fn a_reclaimable_lease_survives_the_reload() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    assert_eq!(
        ask_lease(&mut book, 10, 1, reclaimable(11, "iq2_xs")),
        [grant(1, 11), Action::Persist]
    );
    assert_eq!(book.reconfigure(Moment(20), changed()), []);

    assert_eq!(tick(&mut book, GRACE), [unload("iq2_xs")]);
    assert!(book.lease(LeaseId(11)).is_some());
    assert_eq!(
        book.handle(Moment(GRACE + 1), unloaded("iq2_xs")),
        [load("iq2_xs")]
    );
    assert!(book.lease(LeaseId(11)).is_some());
    assert_eq!(book.handle(Moment(GRACE + 2), loaded("iq2_xs")), []);
    assert!(book.lease(LeaseId(11)).is_some());
}

#[test]
fn a_reclaimable_lease_ends_reclaimed_when_the_reload_fails_for_good() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    let _ = ask_lease(&mut book, 10, 1, reclaimable(11, "iq2_xs"));
    let _ = book.reconfigure(Moment(20), changed());
    let _ = tick(&mut book, GRACE);
    let _ = book.handle(Moment(GRACE + 1), unloaded("iq2_xs"));
    let failed = |error: &str| Event::LoadFailed {
        model: m("iq2_xs"),
        error: error.to_owned(),
    };

    // The one retry keeps the lease.
    assert_eq!(
        book.handle(Moment(GRACE + 2), failed("first")),
        [load("iq2_xs")]
    );
    assert!(book.lease(LeaseId(11)).is_some());
    assert_eq!(
        book.handle(Moment(GRACE + 3), failed("second")),
        [ended(11, Ended::Reclaimed), Action::Persist]
    );
    assert!(book.lease(LeaseId(11)).is_none());
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Unloaded));
    // A model whose reload failed is not reloaded again, nor loaded for nothing.
    assert_eq!(tick(&mut book, 10 * GRACE), []);
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Unloaded));
}

/// The default models without iq2_xs, which the config stops naming.
fn without_iq2_xs() -> Arc<Config> {
    test_support::config(&test_support::HOST_AND_MODELS.replace(
        r#"[models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "37G"
idle = "2h"

"#,
        "",
    ))
}

#[test]
fn a_reclaimable_lease_ends_reclaimed_when_its_model_leaves_the_config_during_the_reload() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    let _ = ask_lease(&mut book, 10, 1, reclaimable(11, "iq2_xs"));
    let _ = book.reconfigure(Moment(20), changed());
    assert_eq!(tick(&mut book, GRACE), [unload("iq2_xs")]);

    // The reload is under way when the config stops naming the model.
    let _ = book.reconfigure(Moment(GRACE + 1), without_iq2_xs());
    assert!(book.lease(LeaseId(11)).is_some());
    assert_eq!(
        book.handle(Moment(GRACE + 2), unloaded("iq2_xs")),
        [ended(11, Ended::Reclaimed), Action::Persist]
    );

    assert!(book.lease(LeaseId(11)).is_none());
    assert_eq!(book.state(&m("iq2_xs")), None);
}

#[test]
fn a_reclaimable_lease_ends_reclaimed_when_its_model_leaves_the_config_between_reload_attempts() {
    let mut book = book();
    warm(&mut book, 0, "iq2_xs");
    warm(&mut book, 0, "laya");
    let _ = ask_lease(&mut book, 10, 1, reclaimable(11, "iq2_xs"));
    let _ = book.reconfigure(Moment(20), changed());
    assert_eq!(tick(&mut book, GRACE), [unload("iq2_xs")]);
    assert_eq!(
        book.handle(Moment(GRACE + 1), unloaded("iq2_xs")),
        [load("iq2_xs")]
    );
    // Its RAM grows past what fits beside laya while it loads, so the retry cannot start.
    let grown = test_support::config(
        &test_support::HOST_AND_MODELS
            .replace("ram = \"37G\"", "ram = \"60G\"")
            .replace(
                IQ2_XS_ENV,
                r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
            ),
    );
    let _ = book.reconfigure(Moment(GRACE + 2), grown);
    let _ = book.handle(
        Moment(GRACE + 3),
        Event::LoadFailed {
            model: m("iq2_xs"),
            error: "first".to_owned(),
        },
    );
    // The retry is owed and claims its room.
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Reserved));
    assert!(book.lease(LeaseId(11)).is_some());

    assert_eq!(
        book.reconfigure(Moment(GRACE + 4), without_iq2_xs()),
        [ended(11, Ended::Reclaimed), Action::Persist]
    );
    assert!(book.lease(LeaseId(11)).is_none());
    assert_eq!(book.state(&m("iq2_xs")), None);
}

#[test]
fn a_request_arriving_during_the_reload_waits_for_the_load() {
    let mut book = stale_iq2_xs();
    let _ = tick(&mut book, GRACE);

    assert_eq!(
        ask(&mut book, GRACE + 1, 1, "iq2_xs", Priority::Interactive),
        [waiting(1, Reason::Draining { model: m("iq2_xs") })]
    );
    assert_eq!(
        book.handle(Moment(GRACE + 2), unloaded("iq2_xs")),
        [
            load("iq2_xs"),
            waiting_until(1, loading("iq2_xs"), GRACE + 2)
        ]
    );
    assert_eq!(
        book.handle(Moment(GRACE + 3), loaded("iq2_xs")),
        [forward(1, "iq2_xs")]
    );
}

#[test]
fn no_waiter_evicts_a_model_that_is_reloading() {
    let mut book = stale_iq2_xs();
    let _ = tick(&mut book, GRACE);

    // iq3_s takes all the VRAM too, but waits behind the reload instead of taking the room.
    assert_eq!(
        ask(&mut book, GRACE + 1, 1, "iq3_s", Priority::Interactive),
        [waiting(1, behind("iq2_xs"))]
    );
    assert_eq!(
        book.handle(Moment(GRACE + 2), unloaded("iq2_xs")),
        [
            load("iq2_xs"),
            waiting_until(1, behind("iq2_xs"), GRACE + 2)
        ]
    );
    assert_eq!(book.state(&m("iq2_xs")), Some(State::Loading));
    assert_eq!(book.state(&m("iq3_s")), Some(State::Unloaded));
}

#[test]
fn a_stale_model_counts_the_larger_of_its_figures_until_the_reload_and_the_new_ones_after() {
    let ram = |book: &Book| model_view(book, 10, "iq2_xs").map(|view| view.footprint.ram);
    let with_ram = |ram: &str| {
        test_support::config(&test_support::HOST_AND_MODELS.replace(
            "vram = \"all\"\nram = \"37G\"",
            &format!("vram = \"all\"\nram = \"{ram}\""),
        ))
    };
    for (new, counted_while_stale, counted_after) in
        [("30G", 37 * GIB, 30 * GIB), ("40G", 40 * GIB, 40 * GIB)]
    {
        let mut book = book();
        warm(&mut book, 0, "iq2_xs");
        let _ = book.reconfigure(Moment(10), with_ram(new));
        // Only launch settings make a model stale, so these figures alone leave it loaded as it is.
        assert!(!stale(&book, "iq2_xs"));
        assert_eq!(ram(&book), Some(counted_while_stale));
        let _ = book.reconfigure(Moment(20), {
            let toml = test_support::HOST_AND_MODELS
                .replace(
                    "vram = \"all\"\nram = \"37G\"",
                    &format!("vram = \"all\"\nram = \"{new}\""),
                )
                .replace(
                    IQ2_XS_ENV,
                    r#"backend = { sheep = "iq2_xs", env = { CONTEXT = "65536" } }"#,
                );
            test_support::config(&toml)
        });
        assert!(stale(&book, "iq2_xs"));
        assert_eq!(ram(&book), Some(counted_while_stale));

        let _ = tick(&mut book, GRACE);
        let _ = book.handle(Moment(GRACE + 1), unloaded("iq2_xs"));
        assert_eq!(ram(&book), Some(counted_after));
        let _ = book.handle(Moment(GRACE + 2), loaded("iq2_xs"));
        assert_eq!(ram(&book), Some(counted_after));
        assert!(!stale(&book, "iq2_xs"));
    }
}
