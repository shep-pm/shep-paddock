//! The pane as the shepherd reads it, and its revoke against a real engine.

use std::time::Duration;

use shep_client::{
    dogs::{Pane, Section, Table},
    shep_core::dogs::pane::Fields,
};
use tokio::{task::LocalSet, time::timeout};

use super::*;
use crate::{
    backend::Backends,
    book::{Hold, LeaseId, LeaseView, Leased, ModelView, Moment, Priority, Revocation, State},
    config::{ClientName, ModelName},
    engine::{EngineHandle, LeaseEvent, LeaseEvents, LeaseRequest, Start, channel, run},
    footprint::{Footprint, Vram},
    survey::Measured,
    test_support::{FakeShepherd, config},
};

const LIMIT: Duration = Duration::from_secs(10);

fn host() -> Host {
    Host {
        vram: 24 << 30,
        ram: 64 << 30,
    }
}

fn clock() -> Clock {
    Clock::started_at("2026-10-04T10:00:00Z".parse().expect("a timestamp"))
}

fn model(name: &str, state: State) -> ModelView {
    ModelView {
        name: ModelName::from(name),
        state,
        in_flight: 0,
        last_used: Moment(0),
        held_by: Vec::new(),
        unknown: false,
        stray: false,
        placement: None,
        footprint: Footprint {
            vram: Vram::None,
            ram: 0,
        },
        measured: Measured::default(),
        measured_peak: Measured::default(),
        drift: false,
        sequences: None,
        turn_ram: None,
        stale: false,
    }
}

fn lease(id: u64, client: &str, model: Option<&str>) -> LeaseView {
    LeaseView {
        id: LeaseId(id),
        client: ClientName::from(client),
        model: model.map(ModelName::from),
        footprint: None,
        pid: None,
        priority: Priority::Interactive,
        since: Moment(0),
        expected_until: None,
        note: None,
        hold: Hold::Connection,
        attached: true,
        reclaimable: false,
        last_activity: Moment(0),
        in_use: false,
        release_if_idle: None,
        revoked: None,
        measured: Measured::default(),
        drift: false,
        energy_mwh: None,
    }
}

fn snapshot(models: Vec<ModelView>, leases: Vec<LeaseView>) -> Snapshot {
    Snapshot {
        models,
        leases,
        waiters: Vec::new(),
        errors: Vec::new(),
        declared: Footprint {
            vram: Vram::Bytes(20 << 30),
            ram: 4 << 30,
        },
        unaccounted_vram: None,
    }
}

fn fields<'a>(pane: &'a Pane, id: &str) -> &'a Fields {
    pane.sections
        .iter()
        .find_map(|section| match section {
            Section::Fields(fields) if fields.id == id => Some(fields),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fields section {id}: {pane:?}"))
}

fn table<'a>(pane: &'a Pane, id: &str) -> &'a Table {
    pane.sections
        .iter()
        .find_map(|section| match section {
            Section::Table(table) if table.id == id => Some(table),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no table {id}: {pane:?}"))
}

fn value<'a>(fields: &'a Fields, label: &str) -> &'a str {
    fields
        .fields
        .iter()
        .find(|field| field.label == label)
        .unwrap_or_else(|| panic!("no field {label}"))
        .value
        .as_str()
}

fn cell<'a>(table: &'a Table, key: &str, column: &str) -> &'a str {
    let at = table
        .columns
        .iter()
        .position(|name| name == column)
        .unwrap_or_else(|| panic!("no column {column}: {:?}", table.columns));
    let row = table
        .rows
        .iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key}"));
    row.cells[at].as_str()
}

#[test]
fn the_pane_shows_the_hosts_vram_and_ram() {
    let pane = build(&snapshot(Vec::new(), Vec::new()), &host(), &clock());

    let host = fields(&pane, "host");
    assert_eq!(value(host, "VRAM"), "20 GiB of 24 GiB declared");
    assert_eq!(value(host, "RAM"), "4 GiB of 64 GiB declared");
}

#[test]
fn the_pane_shows_vram_no_model_accounts_for() {
    let mut snapshot = snapshot(Vec::new(), Vec::new());
    snapshot.unaccounted_vram = Some(2 << 30);

    let pane = build(&snapshot, &host(), &clock());

    assert_eq!(value(fields(&pane, "host"), "VRAM unaccounted"), "2 GiB");
}

#[test]
fn each_model_is_a_row_with_its_state() {
    let loaded = ModelView {
        footprint: Footprint {
            vram: Vram::Bytes(22 << 30),
            ram: 4 << 30,
        },
        in_flight: 2,
        held_by: vec![ClientName::from("bench-01")],
        ..model("qwen3.8:27b", State::Loaded)
    };
    let big = ModelView {
        footprint: Footprint {
            vram: Vram::All,
            ram: 37 << 30,
        },
        ..model("iq2_xs", State::Loading)
    };
    let snapshot = snapshot(
        vec![loaded, big, model("laya", State::Unloaded)],
        Vec::new(),
    );

    let pane = build(&snapshot, &host(), &clock());

    let models = table(&pane, "models");
    assert_eq!(models.rows.len(), 3);
    assert_eq!(cell(models, "qwen3.8:27b", "MODEL"), "qwen3.8:27b");
    assert_eq!(cell(models, "qwen3.8:27b", "STATE"), "loaded");
    assert_eq!(cell(models, "qwen3.8:27b", "VRAM"), "22 GiB");
    assert_eq!(cell(models, "qwen3.8:27b", "RAM"), "4 GiB");
    assert_eq!(cell(models, "qwen3.8:27b", "IN FLIGHT"), "2");
    assert_eq!(cell(models, "qwen3.8:27b", "HELD BY"), "bench-01");
    assert_eq!(cell(models, "iq2_xs", "STATE"), "loading");
    assert_eq!(cell(models, "iq2_xs", "VRAM"), "all");
    assert_eq!(cell(models, "laya", "STATE"), "unloaded");
    assert!(
        models.row_actions.is_empty(),
        "a model has nothing to revoke"
    );
}

#[test]
fn each_lease_is_a_row_keyed_by_its_id() {
    let mut ended = lease(9, "mac-sessions", None);
    ended.revoked = Some(Revocation {
        by: ClientName::from("operator"),
        note: None,
    });
    let snapshot = snapshot(
        Vec::new(),
        vec![lease(7, "bench-01", Some("iq2_xs")), ended],
    );

    let pane = build(&snapshot, &host(), &clock());

    let leases = table(&pane, "leases");
    assert_eq!(
        leases.rows.iter().map(|row| &*row.key).collect::<Vec<_>>(),
        ["L7", "L9"]
    );
    assert_eq!(cell(leases, "L7", "CLIENT"), "bench-01");
    assert_eq!(cell(leases, "L7", "MODEL"), "iq2_xs");
    assert_eq!(cell(leases, "L7", "STATE"), "granted");
    assert_eq!(cell(leases, "L9", "MODEL"), "-");
    assert_eq!(cell(leases, "L9", "STATE"), "revoked");
    let [revoke] = leases.row_actions.as_slice() else {
        panic!("one row action: {:?}", leases.row_actions);
    };
    assert_eq!(revoke.action, "revoke");
    assert!(revoke.destructive, "a revoke asks first");
}

#[test]
fn the_answer_reads_back_as_the_pane_it_was_built_from() {
    let latest = Latest::new();
    latest.publish(snapshot(
        vec![model("laya", State::Unloaded)],
        vec![lease(7, "bench-01", None)],
    ));

    let body = answer(&latest, &host(), &clock(), None);

    let read = Pane::read(&body).expect("shep reads the answer");
    assert_eq!(
        read.sections,
        build(
            &snapshot(
                vec![model("laya", State::Unloaded)],
                vec![lease(7, "bench-01", None)]
            ),
            &host(),
            &clock()
        )
        .sections
    );
}

#[test]
fn before_the_first_snapshot_the_pane_says_it_is_starting() {
    let body = answer(&Latest::new(), &host(), &clock(), None);

    let read = Pane::read(&body).expect("shep reads the answer");
    assert_eq!(value(fields(&read, "host"), "STATUS"), "starting");
}

const TWO_CLIENTS: &str = r#"
listen = "127.0.0.1:0"

[host]
vram = "24G"
ram = "64G"

[[clients]]
name = "bench-01"
key = "k-bench"

[[clients]]
name = "mac-sessions"
key = "k-mac"
protected = true

[backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[models.laya]
backend = "ollama"
name = "laya"
apis = ["openai"]
ram = "5G"
idle = "2h"
"#;

fn bare_lease() -> LeaseRequest {
    LeaseRequest {
        leased: Leased::Bare {
            footprint: Footprint {
                vram: Vram::Bytes(1 << 30),
                ram: 1 << 30,
            },
            pid: None,
        },
        priority: Priority::Batch,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: None,
        reclaimable: false,
        release_if_idle: None,
    }
}

/// Runs `body` against a live engine, which stops with it
async fn with_engine<F, Fut>(body: F)
where
    F: FnOnce(EngineHandle) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (engine, inbox) = channel();
    let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
    let local = LocalSet::new();
    local.spawn_local(run(
        config(TWO_CLIENTS),
        backends,
        Start::default(),
        inbox,
        shep_client::dogs::Stop::never(),
    ));
    local.run_until(body(engine)).await;
}

/// Takes a lease as `client` and waits until it is granted
async fn granted(engine: &EngineHandle, client: &str) -> (LeaseId, LeaseEvents) {
    let mut events = engine
        .take_lease(ClientName::from(client), bare_lease())
        .await;
    loop {
        match timeout(LIMIT, events.recv()).await.expect("in time") {
            Some(LeaseEvent::Granted { lease }) => return (lease, events),
            Some(_) => {}
            None => panic!("the stream closed before the grant"),
        }
    }
}

fn params(row: &str) -> String {
    serde_json::json!({ "row": row }).to_string()
}

#[tokio::test]
async fn revoke_ends_the_lease_a_row_names() {
    with_engine(|engine| async move {
        let (lease, mut events) = granted(&engine, "bench-01").await;
        let latest = Latest::new();

        let said = revoke(&engine, &latest, Some(&params(&render_id(lease)))).await;

        assert_eq!(said, format!("revoked {lease}"));
        let heard = timeout(LIMIT, events.recv()).await.expect("in time");
        let Some(LeaseEvent::Ended(_) | LeaseEvent::Revoked(_)) = heard else {
            panic!("the holder was not told: {heard:?}");
        };
        let snapshot = engine.snapshot().await;
        assert!(
            snapshot.leases.iter().all(|view| view.id != lease
                || view
                    .revoked
                    .as_ref()
                    .is_some_and(|revoked| revoked.by.as_str() == OPERATOR)),
            "the operator is the revoker: {snapshot:?}"
        );
        let pulled = Pane::read(&answer(&latest, &host(), &engine.clock(), None)).expect("a pane");
        assert_eq!(
            cell(table(&pulled, "leases"), &render_id(lease), "STATE"),
            "revoked",
            "the pane's next pull sees the revoke"
        );
    })
    .await;
}

#[tokio::test]
async fn revoke_refuses_a_row_that_names_no_lease() {
    with_engine(|engine| async move {
        let (lease, _events) = granted(&engine, "bench-01").await;
        let latest = Latest::new();

        for text in [
            Some(params("L99")),
            Some(params("nope")),
            Some("{}".to_owned()),
            None,
        ] {
            let said = revoke(&engine, &latest, text.as_deref()).await;
            assert_eq!(said, "no such lease", "for {text:?}");
        }

        let snapshot = engine.snapshot().await;
        let held = snapshot
            .leases
            .iter()
            .find(|view| view.id == lease)
            .expect("the lease is still listed");
        assert_eq!(held.revoked, None);
    })
    .await;
}

#[tokio::test]
async fn revoke_refuses_a_protected_clients_lease() {
    with_engine(|engine| async move {
        let (lease, _events) = granted(&engine, "mac-sessions").await;
        let latest = Latest::new();

        let said = revoke(&engine, &latest, Some(&params(&render_id(lease)))).await;

        assert_eq!(said, "the lease is held by a protected client");
        let snapshot = engine.snapshot().await;
        let held = snapshot
            .leases
            .iter()
            .find(|view| view.id == lease)
            .expect("the lease is still listed");
        assert_eq!(held.revoked, None);
    })
    .await;
}

/// The channel runs a handler on a thread of its own, while the engine runs on the dog's one
/// runtime thread, which the handler's wait must leave free.
#[tokio::test]
async fn a_handler_thread_waits_on_the_engine_without_holding_the_runtime() {
    with_engine(|engine| async move {
        let (lease, _events) = granted(&engine, "bench-01").await;
        let latest = Latest::new();
        let runtime = tokio::runtime::Handle::current();
        let (done, said) = tokio::sync::oneshot::channel();
        let (asked, text) = (engine.clone(), params(&render_id(lease)));

        std::thread::spawn(move || {
            let said = runtime.block_on(revoke(&asked, &latest, Some(&text)));
            let _ = done.send(said);
        });

        let said = timeout(LIMIT, said)
            .await
            .expect("in time")
            .expect("an answer");
        assert_eq!(said, format!("revoked {lease}"));
    })
    .await;
}
