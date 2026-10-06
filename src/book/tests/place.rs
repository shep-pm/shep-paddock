use super::*;
use crate::{config::PlacementName, test_support};

fn p(name: &str) -> Option<PlacementName> {
    Some(PlacementName::from(name))
}

fn placed_book() -> Book {
    Book::new(test_support::placed())
}

/// The placed laya, then a model that needs no VRAM and excludes laya.
fn with_embed() -> Book {
    book_from(&format!(
        "{}\n[models.embed]\nbackend = {{ sheep = \"embed\" }}\nurl = \"http://127.0.0.1:8002\"\n\
         ram = \"10G\"\nexcludes = [\"laya\"]\nidle = \"1h\"\n",
        test_support::placed_toml()
    ))
}

// qwen and big fit together, and laya fits beside neither: its GPU placement passes the
// card beside qwen, its RAM placement passes the host's RAM beside both.
const TIGHT: &str = r#"
[host]
vram = "24564M"
ram = "63439M"

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"
vram = "22323M"
ram = "4G"
idle = "2h"

[models.big]
backend = { sheep = "big" }
url = "http://127.0.0.1:8090"
ram = "56G"
idle = "2h"

[models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
idle = "8h"

[[models.laya.placements]]
name = "gpu"
vram = "6G"
ram = "2G"

[[models.laya.placements]]
name = "ram"
ram = "5G"
"#;

// laya's RAM placement, as `placed_toml` writes it, to take out of a reloaded config.
const RAM_PLACEMENT: &str = "\n[[models.laya.placements]]\nname = \"ram\"\nram = \"5G\"\n\
    script = \"/opt/laya/venv/bin/laya-serve\"\n\
    env = { LAYA_DEVICE = \"cpu\", CUDA_VISIBLE_DEVICES = \"\" }\n";

fn ram_only() -> Footprint {
    Footprint {
        vram: Vram::None,
        ram: 5 * GIB,
    }
}

fn largest() -> Footprint {
    Footprint {
        vram: Vram::Bytes(6 * GIB),
        ram: 5 * GIB,
    }
}

#[test]
fn laya_loads_on_the_gpu_while_the_card_is_free() {
    let mut book = placed_book();
    let actions = ask(&mut book, 0, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_000)
        ]
    );
    assert_eq!(book.placement(&m("laya")), p("gpu"));
}

#[test]
fn laya_loads_in_ram_beside_qwen_instead_of_evicting_it() {
    let mut book = placed_book();
    warm(&mut book, 0, QWEN);
    let actions = ask(&mut book, 10, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_010)
        ]
    );
    assert_eq!(book.placement(&m("laya")), p("ram"));
    assert_eq!(book.state(&m(QWEN)), Some(State::Loaded));
}

/// big is the least recently used, and evicting it would let laya load in RAM. The GPU
/// placement comes first and evicting qwen makes room for it, so qwen goes.
#[test]
fn the_first_placement_with_an_eviction_set_wins_over_the_least_recently_used() {
    let mut book = book_from(TIGHT);
    warm(&mut book, 0, "big");
    warm(&mut book, 10, QWEN);
    let actions = ask(&mut book, 20, 1, "laya", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m(QWEN)), waiting(1, loading("laya"))]
    );
    assert_eq!(book.placement(&m("laya")), p("gpu"));
    assert_eq!(book.state(&m("big")), Some(State::Loaded));

    let actions = book.handle(Moment(30), Event::Unloaded { model: m(QWEN) });
    assert_eq!(
        actions,
        vec![
            Action::Load(m("laya")),
            waiting_until(1, loading("laya"), 60_030)
        ]
    );
}

#[test]
fn a_running_model_stays_where_it_loaded_when_the_card_frees() {
    let mut book = placed_book();
    warm(&mut book, 0, QWEN);
    warm(&mut book, 10, "laya");
    assert_eq!(book.placement(&m("laya")), p("ram"));

    assert_eq!(tick(&mut book, 7_200_000), vec![Action::Unload(m(QWEN))]);
    assert_eq!(
        book.handle(Moment(7_200_000), Event::Unloaded { model: m(QWEN) }),
        vec![]
    );
    assert_eq!(
        ask(&mut book, 7_200_100, 2, "laya", Priority::Interactive),
        vec![forward(2, "laya")]
    );
    assert_eq!(
        book.placement(&m("laya")),
        p("ram"),
        "a running model is never moved"
    );

    let _ = book.handle(
        Moment(7_200_100),
        Event::RequestFinished { model: m("laya") },
    );
    assert_eq!(tick(&mut book, 36_000_100), vec![Action::Unload(m("laya"))]);
    let _ = book.handle(Moment(36_000_100), Event::Unloaded { model: m("laya") });
    assert_eq!(book.placement(&m("laya")), None);
    let _ = ask(&mut book, 36_000_200, 3, "laya", Priority::Interactive);
    assert_eq!(
        book.placement(&m("laya")),
        p("gpu"),
        "the next load chooses again"
    );
}

/// RAM alone would let embed load beside laya in RAM; the exclusion names laya, not a placement.
#[test]
fn an_exclusion_covers_every_placement() {
    let mut book = with_embed();
    warm(&mut book, 0, QWEN);
    warm(&mut book, 10, "laya");
    assert_eq!(book.placement(&m("laya")), p("ram"));
    let actions = ask(&mut book, 20, 1, "embed", Priority::Interactive);
    assert_eq!(
        actions,
        vec![Action::Unload(m("laya")), waiting(1, loading("embed"))]
    );
}

#[test]
fn a_placed_model_counts_at_its_placements_figures() {
    let mut book = placed_book();
    warm(&mut book, 0, QWEN);
    warm(&mut book, 10, "laya");
    assert_eq!(
        book.snapshot(Moment(20)).declared,
        Footprint {
            vram: Vram::Bytes(22_323 * MIB),
            ram: 9 * GIB
        }
    );
}

#[test]
fn a_reload_that_removes_a_loaded_placement_keeps_counting_what_it_loaded_with() {
    let mut book = placed_book();
    warm(&mut book, 0, QWEN);
    warm(&mut book, 10, "laya");
    let gpu_only = test_support::placed_toml().replace(RAM_PLACEMENT, "");
    assert_ne!(
        gpu_only,
        test_support::placed_toml(),
        "the RAM placement text moved"
    );

    let actions = book.reconfigure(Moment(20), test_support::config(&gpu_only));

    assert_eq!(
        actions,
        vec![],
        "nothing loads or unloads because figures changed"
    );
    assert_eq!(book.state(&m("laya")), Some(State::Loaded));
    assert_eq!(book.placement(&m("laya")), p("ram"));
    assert_eq!(
        book.snapshot(Moment(30)).declared,
        Footprint {
            vram: Vram::Bytes(22_323 * MIB),
            ram: 9 * GIB
        },
        "what it loaded with: no VRAM it does not hold, and no less RAM than it does"
    );
}

#[test]
fn restore_counts_a_found_model_at_its_saved_placement() {
    let mut book = placed_book();
    let found = Found {
        model: m("laya"),
        footprint: ram_only(),
        placement: p("ram"),
    };
    assert_eq!(
        book.restore(Moment(1_000), vec![found], &[], vec![]),
        vec![]
    );
    assert_eq!(book.placement(&m("laya")), p("ram"));
    assert_eq!(
        book.snapshot(Moment(1_000)).declared,
        Footprint {
            vram: Vram::Bytes(0),
            ram: 5 * GIB
        }
    );
}

#[test]
fn restore_without_a_placement_counts_at_the_largest() {
    let mut book = placed_book();
    let found = Found {
        model: m("laya"),
        footprint: largest(),
        placement: None,
    };
    let _ = book.restore(Moment(1_000), vec![found], &[], vec![]);
    assert_eq!(book.placement(&m("laya")), None);
    assert_eq!(
        book.snapshot(Moment(1_000)).declared,
        Footprint {
            vram: Vram::Bytes(6 * GIB),
            ram: 5 * GIB
        }
    );
}

#[test]
fn the_snapshot_shows_a_models_placement_and_figures_until_it_unloads() {
    let mut book = placed_book();
    warm(&mut book, 0, QWEN);
    warm(&mut book, 10, "laya");
    let view = model_view(&book, 20, "laya").expect("laya");
    assert_eq!((view.placement, view.footprint), (p("ram"), ram_only()));

    let _ = book.handle(Moment(30), Event::BackendExited { model: m("laya") });
    let _ = book.handle(Moment(40), Event::Unloaded { model: m("laya") });
    let view = model_view(&book, 50, "laya").expect("laya");
    assert_eq!(view.placement, None);
}
