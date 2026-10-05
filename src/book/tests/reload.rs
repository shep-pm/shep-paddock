use super::*;
use crate::footprint::Vram;

#[test]
fn a_removed_model_stays_while_leased() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    assert_eq!(
        take(&mut book, 10, 1, 11, QWEN, None),
        [grant(1, 11), Action::Persist]
    );
    let without = test_support::config(&test_support::HOST_AND_MODELS.replace(QWEN_SECTION, ""));

    assert_eq!(book.reconfigure(Moment(20), without), []);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
    assert!(book.lease(LeaseId(11)).is_some());
    assert_eq!(tick(&mut book, 9 * 3_600_000), []);
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
    assert_eq!(
        ask(
            &mut book,
            9 * 3_600_000 + 1,
            2,
            "iq2_xs",
            Priority::Interactive
        ),
        [refuse(2, held_by_bench(QWEN, 11, 10))]
    );
    assert_eq!(
        ask(&mut book, 9 * 3_600_000 + 2, 3, QWEN, Priority::Interactive),
        [fail(3, "no model named qwen3.8:27b")]
    );
}

#[test]
fn a_removed_model_unloads_once_nothing_names_it() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let _ = take(&mut book, 10, 1, 11, QWEN, None);
    assert_eq!(
        ask(&mut book, 20, 2, QWEN, Priority::Interactive),
        [forward(2, QWEN)]
    );
    let toml = test_support::HOST_AND_MODELS.replace(QWEN_SECTION, "");

    assert_eq!(
        book.reconfigure(Moment(30), test_support::config(&toml)),
        []
    );
    assert_eq!(
        book.handle(Moment(40), Event::LeaseReleased { lease: LeaseId(11) }),
        [ended(11, Ended::Released), Action::Persist]
    );
    assert_eq!(
        book.handle(Moment(50), Event::RequestFinished { model: m(QWEN) }),
        [Action::Unload(m(QWEN))]
    );
    assert_eq!(
        book.handle(Moment(60), Event::Unloaded { model: m(QWEN) }),
        []
    );
    assert_eq!(book.state(&m(QWEN)), None);
    assert_eq!(model_view(&book, 60, QWEN), None);
}

#[test]
fn a_removed_model_that_nothing_names_unloads_at_the_reload() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let laya = r#"[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "k-laya"
ready = { path = "/health", field = "loaded" }
ram = "5G"
idle = "8h"
"#;
    let toml = test_support::HOST_AND_MODELS
        .replace("excludes = [\"laya\"]\n", "")
        .replace(laya, "");
    let config = test_support::config(&toml);
    assert!(!config.models.contains_key(&m("laya")));

    assert_eq!(
        book.reconfigure(Moment(10), config),
        [Action::Unload(m("laya"))]
    );
}

#[test]
fn a_waiter_on_a_removed_model_fails() {
    let mut book = book();
    let _ = ask(&mut book, 0, 1, QWEN, Priority::Interactive);
    let toml = test_support::HOST_AND_MODELS.replace(QWEN_SECTION, "");

    assert_eq!(
        book.reconfigure(Moment(10), test_support::config(&toml)),
        [fail(1, "qwen3.8:27b was removed from the config")]
    );
    assert_eq!(
        book.handle(Moment(20), Event::Loaded { model: m(QWEN) }),
        [Action::Unload(m(QWEN))]
    );
    let _ = book.handle(Moment(30), Event::Unloaded { model: m(QWEN) });
    assert_eq!(book.state(&m(QWEN)), None);
}

#[test]
fn reload_does_not_unload_a_model_whose_figures_grew() {
    let mut book = book();
    warm(&mut book, 0, QWEN);
    let grown = test_support::HOST_AND_MODELS.replace("vram = \"22323M\"", "vram = \"24000M\"");
    let declared = |vram| Footprint {
        vram: Vram::Bytes(vram * MIB),
        ram: 4 * GIB,
    };

    assert_eq!(
        book.reconfigure(Moment(10), test_support::config(&grown)),
        []
    );
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
    assert_eq!(book.snapshot(Moment(10)).declared, declared(24_000));

    assert_eq!(tick(&mut book, 7_200_000), [Action::Unload(m(QWEN))]);
    let _ = book.handle(Moment(7_200_001), Event::Unloaded { model: m(QWEN) });
    let _ = ask(&mut book, 7_200_002, 1, QWEN, Priority::Interactive);
    let _ = book.handle(Moment(7_200_003), Event::Loaded { model: m(QWEN) });
    assert_eq!(book.snapshot(Moment(7_200_003)).declared, declared(24_000));
}

#[test]
fn a_shorter_idle_applies_to_a_loaded_model() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let toml = test_support::HOST_AND_MODELS
        .replace("ram = \"5G\"\nidle = \"8h\"", "ram = \"5G\"\nidle = \"1m\"");

    assert_eq!(
        book.reconfigure(Moment(10), test_support::config(&toml)),
        []
    );
    assert_eq!(book.next_deadline(), Some(Moment(60_000)));
    assert_eq!(tick(&mut book, 60_000), [Action::Unload(m("laya"))]);
}

#[test]
fn a_reserved_model_makes_room_again_under_a_new_config() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    let _ = ask(&mut book, 1, 1, "iq2_xs", Priority::Interactive);
    let _ = book.handle(Moment(2), Event::Loaded { model: m("iq2_xs") });
    assert_eq!(
        ask(&mut book, 10, 2, QWEN, Priority::Interactive),
        [waiting(2, loading(QWEN))]
    );
    let toml = test_support::HOST_AND_MODELS
        .replace("ram = \"4G\"\n", "ram = \"4G\"\nexcludes = [\"laya\"]\n");

    assert_eq!(
        book.reconfigure(Moment(20), test_support::config(&toml)),
        [Action::Unload(m("laya"))]
    );
    assert_eq!(book.state(&m(QWEN)), Some(State::Reserved));
    let _ = book.handle(Moment(30), Event::Unloaded { model: m("laya") });
    assert_eq!(
        book.handle(Moment(40), Event::RequestFinished { model: m("iq2_xs") }),
        [Action::Unload(m("iq2_xs"))]
    );
    assert_eq!(
        book.handle(Moment(50), Event::Unloaded { model: m("iq2_xs") }),
        [
            Action::Load(m(QWEN)),
            waiting_until(2, loading(QWEN), 60_050)
        ]
    );
}

#[test]
fn restored_connection_leases_get_the_reconnect_window() {
    let mut abandoned = book();
    let loaded = vec![(m("iq2_xs"), footprint(&abandoned, "iq2_xs"))];
    let leases = vec![restored(lease_ask(7, "iq2_xs"), 0)];

    assert_eq!(
        abandoned.restore(Moment(1_000), loaded.clone(), &[], leases.clone()),
        []
    );
    let view = abandoned.lease(LeaseId(7));
    assert_eq!(view.as_ref().map(|lease| lease.attached), Some(false));
    assert_eq!(view.map(|lease| lease.since), Some(Moment(0)));
    assert_eq!(abandoned.next_deadline(), Some(Moment(61_000)));
    assert_eq!(tick(&mut abandoned, 60_999), []);
    assert_eq!(
        tick(&mut abandoned, 61_000),
        [ended(7, Ended::Abandoned), Action::Persist]
    );

    let mut attached = book();
    let _ = attached.restore(Moment(1_000), loaded, &[], leases);
    let attach = Event::HolderAttached { lease: LeaseId(7) };
    assert_eq!(attached.handle(Moment(30_000), attach), []);
    assert_eq!(tick(&mut attached, 3_600_000), []);
    assert_eq!(attached.state(&m("iq2_xs")), Some(State::Loaded));
}

#[test]
fn restored_heartbeat_leases_get_a_fresh_ttl() {
    let mut book = book();
    let loaded = vec![(m("laya"), footprint(&book, "laya"))];
    let leases = vec![restored(heartbeat(7, "laya", 60), 0)];

    assert_eq!(book.restore(Moment(100_000), loaded, &[], leases), []);
    assert_eq!(
        book.lease(LeaseId(7)).map(|lease| lease.attached),
        Some(true)
    );
    assert_eq!(book.next_deadline(), Some(Moment(160_000)));
    assert_eq!(tick(&mut book, 159_999), []);
    assert_eq!(
        tick(&mut book, 160_000),
        [ended(7, Ended::Expired), Action::Persist]
    );
}

#[test]
fn an_unknown_model_is_counted_and_reclaimable() {
    let mut book = book();
    let stray = Footprint {
        vram: Vram::Bytes(20 * GIB),
        ram: 2 * GIB,
    };
    let loaded = vec![(m("stray"), stray), (m("laya"), footprint(&book, "laya"))];

    assert_eq!(book.restore(Moment(1_000), loaded, &[], vec![]), []);
    let snapshot = book.snapshot(Moment(1_000));
    assert_eq!(
        snapshot.declared,
        Footprint {
            vram: Vram::Bytes(20 * GIB),
            ram: 7 * GIB,
        }
    );
    assert_eq!(
        model_view(&book, 1_000, "stray"),
        Some(ModelView {
            name: m("stray"),
            state: State::Loaded,
            in_flight: 0,
            last_used: Moment(1_000),
            held_by: vec![],
            unknown: true,
        })
    );
    assert_eq!(
        model_view(&book, 1_000, "laya").map(|v| v.unknown),
        Some(false)
    );
    assert_eq!(
        ask(&mut book, 1_001, 1, "stray", Priority::Interactive),
        [fail(1, "no model named stray")]
    );

    // A batch waiter waits out the grace period counted from the restart.
    let grace = Reason::Grace {
        model: m("stray"),
        until: Moment(121_000),
    };
    assert_eq!(
        take(&mut book, 2_000, 2, 12, QWEN, None),
        [waiting_until(2, grace, 181_000)]
    );
    assert_eq!(
        ask(&mut book, 3_000, 3, QWEN, Priority::Interactive),
        [
            Action::Unload(m("stray")),
            waiting(3, loading(QWEN)),
            waiting(2, loading(QWEN)),
        ]
    );
    assert_eq!(
        book.handle(Moment(4_000), Event::Unloaded { model: m("stray") }),
        [
            Action::Load(m(QWEN)),
            waiting_until(3, loading(QWEN), 64_000),
            waiting_until(2, loading(QWEN), 64_000),
        ]
    );
    assert_eq!(book.state(&m("stray")), None);
}

#[test]
fn snapshot_reports_declared_totals() {
    let mut book = book();
    warm(&mut book, 0, "laya");
    warm(&mut book, 1, "iq2_xs");
    let declared = |vram, ram| Footprint {
        vram: Vram::Bytes(vram * MIB),
        ram: ram * GIB,
    };
    assert_eq!(book.snapshot(Moment(2)).declared, declared(24_564, 42));

    assert_eq!(
        ask(&mut book, 10, 1, QWEN, Priority::Interactive),
        [Action::Unload(m("iq2_xs")), waiting(1, loading(QWEN))]
    );
    // Both the Unloading model and the Reserved one count.
    assert_eq!(
        book.snapshot(Moment(10)).declared,
        declared(24_564 + 22_323, 46)
    );

    let _ = book.handle(Moment(20), Event::Unloaded { model: m("iq2_xs") });
    assert_eq!(book.snapshot(Moment(20)).declared, declared(22_323, 9));
}

const TWO_MODELS: &str = r#"
[host]
vram = "24G"
ram = "16G"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.y]
backend = "ollama"
name = "y"
vram = "10G"
ram = "1G"
idle = "1h"

[models.r]
backend = "ollama"
name = "r"
vram = "14G"
ram = "1G"
idle = "1h"
"#;

#[test]
fn a_grown_figure_counts_at_once_against_later_loads() {
    let mut book = book_from(TWO_MODELS);
    let _ = ask(&mut book, 0, 1, "y", Priority::Interactive);
    let _ = book.handle(Moment(0), Event::Loaded { model: m("y") });
    let grown = TWO_MODELS.replace("vram = \"10G\"", "vram = \"12G\"");

    assert_eq!(
        book.reconfigure(Moment(10), test_support::config(&grown)),
        []
    );
    assert_eq!(book.state(&m("y")), Some(State::Loaded));
    assert_eq!(
        ask(&mut book, 20, 2, "r", Priority::Interactive),
        [waiting(2, loading("r"))]
    );
    assert_eq!(book.state(&m("r")), Some(State::Reserved));
    assert_eq!(book.state(&m("y")), Some(State::Evicting));
}

#[test]
fn a_shrunk_figure_counts_until_the_model_unloads() {
    let toml = TWO_MODELS.replace("vram = \"10G\"", "vram = \"12G\"");
    let mut book = book_from(&toml);
    let _ = ask(&mut book, 0, 1, "y", Priority::Interactive);
    let _ = book.handle(Moment(0), Event::Loaded { model: m("y") });

    assert_eq!(
        book.reconfigure(Moment(10), test_support::config(TWO_MODELS)),
        []
    );
    assert_eq!(
        ask(&mut book, 20, 2, "r", Priority::Interactive),
        [waiting(2, loading("r"))]
    );
    assert_eq!(book.state(&m("y")), Some(State::Evicting));
}

#[test]
fn a_removed_model_that_fails_to_load_is_not_retried() {
    let mut book = book();
    assert_eq!(
        ask(&mut book, 0, 1, QWEN, Priority::Interactive)[0],
        Action::Load(m(QWEN))
    );
    let toml = test_support::HOST_AND_MODELS.replace(QWEN_SECTION, "");
    let _ = book.reconfigure(Moment(10), test_support::config(&toml));
    let failed = Event::LoadFailed {
        model: m(QWEN),
        error: "no".to_owned(),
    };

    assert_eq!(book.handle(Moment(20), failed), []);
    assert_eq!(book.state(&m(QWEN)), None);
}
