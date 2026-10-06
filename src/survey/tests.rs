//! Parsing and attribution from output captured on the GPU host, and drift crossing both ways.

use std::collections::BTreeMap;

use shep_client::shep_core::{
    protocol::{Lamb, ProcessInfo},
    status::ProcStatus,
};

use super::{
    drift::{Drifting, drifts},
    gpu::{GpuApp, GpuParseError, GpuReading, reading},
    *,
};
use crate::{
    config::ModelName,
    footprint::{Footprint, Vram},
    test_support::captured::{
        IDLE_TOTALS, PS_QWEN, QWEN_BLOB, QWEN_MANIFEST, QWEN_RUNNER_APP, QWEN_RUNNER_PID,
        STRATA_ENGINE, qwen_runner_args,
    },
};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn gpu(totals: &str, apps: &str) -> GpuReading {
    reading(totals, apps).expect("parses")
}

fn row(name: &str, pid: u32, lambs: &[(u32, &str)], memory: Option<u64>) -> ProcessInfo {
    ProcessInfo::builder(1, name, ProcStatus::Online)
        .pid(Some(pid))
        .lambs(Some(
            lambs
                .iter()
                .map(|(pid, name)| Lamb::new(*pid, *name))
                .collect(),
        ))
        .memory_bytes(memory)
        .build()
}

fn on_sheep(model: &str, declared: Footprint) -> Tracked {
    Tracked {
        model: ModelName::from(model),
        on: Where::Sheep(model.to_owned()),
        declared,
    }
}

fn qwen(blob: Option<&str>) -> Tracked {
    Tracked {
        model: ModelName::from("qwen3.8:27b"),
        on: Where::Ollama {
            blob: blob.map(str::to_owned),
        },
        declared: Footprint {
            vram: Vram::Bytes(22_323 * MIB),
            ram: 4 * GIB,
        },
    }
}

fn runner() -> BTreeMap<u32, Vec<String>> {
    BTreeMap::from([(QWEN_RUNNER_PID, qwen_runner_args())])
}

fn laya_gpu() -> Footprint {
    Footprint {
        vram: Vram::Bytes(6 * GIB),
        ram: 2 * GIB,
    }
}

fn measure_of(model: &str, measures: &Measures) -> Measured {
    measures.models[&ModelName::from(model)]
}

#[test]
fn the_captured_totals_read_as_bytes() {
    assert_eq!(
        gpu(IDLE_TOTALS, ""),
        GpuReading {
            used: 17 * MIB,
            total: 24_564 * MIB,
            apps: vec![]
        }
    );
}

#[test]
fn the_captured_compute_apps_read_with_their_pids() {
    let apps = format!("{QWEN_RUNNER_APP}{STRATA_ENGINE}");
    assert_eq!(
        gpu(IDLE_TOTALS, &apps).apps,
        vec![
            GpuApp {
                pid: 190_784,
                used: 19_542 * MIB
            },
            GpuApp {
                pid: 188_622,
                used: 23_702 * MIB
            }
        ]
    );
}

/// Built, not captured: the host has one GPU.
#[test]
fn two_gpus_sum() {
    let reading = gpu("17 MiB, 24564 MiB\n1000 MiB, 8192 MiB\n", "");
    assert_eq!((reading.used, reading.total), (1_017 * MIB, 32_756 * MIB));
}

#[test]
fn a_row_whose_memory_is_not_a_mib_figure_is_skipped() {
    let apps = format!("4242, /usr/bin/python3, [N/A]\n{QWEN_RUNNER_APP}");
    assert_eq!(
        gpu(IDLE_TOTALS, &apps).apps,
        vec![GpuApp {
            pid: 190_784,
            used: 19_542 * MIB
        }]
    );
}

#[test]
fn an_unreadable_line_is_an_error_not_a_zero() {
    let line = |text: &str| {
        Err(GpuParseError::Line {
            line: text.to_owned(),
        })
    };
    assert_eq!(reading("17 MiB\n", ""), line("17 MiB"));
    assert_eq!(reading("17 MB, 24564 MB\n", ""), line("17 MB, 24564 MB"));
    assert_eq!(reading(IDLE_TOTALS, "not a row\n"), line("not a row"));
    assert_eq!(
        reading(IDLE_TOTALS, "x, /bin/x, 3 MiB\n"),
        line("x, /bin/x, 3 MiB")
    );
    assert_eq!(reading("", ""), Err(GpuParseError::NoGpu));
}

#[test]
fn a_sheep_models_vram_is_its_process_trees_and_its_ram_is_sheps_figure() {
    let flock = [row("laya", 1_000, &[(1_001, "python3")], Some(1_504 * MIB))];
    let reading = gpu(
        "6000 MiB, 24564 MiB\n",
        "1001, /usr/bin/python3, 4000 MiB\n4242, /usr/bin/python3, 1500 MiB\n",
    );
    let tracked = [on_sheep("laya", laya_gpu())];
    let measures = measure(&Inputs {
        tracked: &tracked,
        flock: &flock,
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &BTreeMap::new(),
    });
    assert_eq!(
        measure_of("laya", &measures),
        Measured {
            vram: Some(4_000 * MIB),
            ram: Some(1_504 * MIB)
        }
    );
    assert_eq!(measures.unaccounted_vram, Some(2_000 * MIB));
}

#[test]
fn an_ollama_model_is_measured_by_the_runner_that_loads_its_blob() {
    let reading = gpu("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP);
    let blobs = [QWEN_BLOB.to_owned()];
    let measures = measure(&Inputs {
        tracked: &[qwen(Some(QWEN_BLOB))],
        flock: &[],
        blobs: &blobs,
        gpu: Some(&reading),
        cmdlines: &runner(),
    });
    assert_eq!(
        measure_of("qwen3.8:27b", &measures),
        Measured {
            vram: Some(19_542 * MIB),
            ram: None
        }
    );
    assert_eq!(measures.unaccounted_vram, Some(58 * MIB));
}

#[test]
fn the_captured_ps_digest_is_the_manifest_and_not_the_blob() {
    let ps: serde_json::Value = serde_json::from_str(PS_QWEN).expect("json");
    assert_eq!(ps["models"][0]["digest"].as_str(), Some(QWEN_MANIFEST));
    assert_ne!(QWEN_MANIFEST, QWEN_BLOB);
}

/// The manifest digest `/api/ps` reports is on no command line, so matching it finds nothing.
#[test]
fn the_manifest_digest_alone_attributes_nothing() {
    let reading = gpu("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP);
    let blobs = [QWEN_MANIFEST.to_owned()];
    let measures = measure(&Inputs {
        tracked: &[qwen(Some(QWEN_MANIFEST))],
        flock: &[],
        blobs: &blobs,
        gpu: Some(&reading),
        cmdlines: &runner(),
    });
    assert_eq!(measure_of("qwen3.8:27b", &measures).vram, None);
    assert_eq!(measures.unaccounted_vram, Some(19_600 * MIB));
}

#[test]
fn a_blob_matches_a_whole_argument_and_not_part_of_one() {
    let reading = gpu("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP);
    let prefix = QWEN_BLOB[..8].to_owned();
    let measures = measure(&Inputs {
        tracked: &[qwen(Some(&prefix))],
        flock: &[],
        blobs: std::slice::from_ref(&prefix),
        gpu: Some(&reading),
        cmdlines: &runner(),
    });
    assert_eq!(measure_of("qwen3.8:27b", &measures).vram, None);
}

#[test]
fn a_podman_sheeps_vram_is_unmeasured_and_hides_unaccounted() {
    let flock = [row("iq2_xs", 3_000, &[(3_001, "podman")], Some(80 * MIB))];
    let reading = gpu("23800 MiB, 24564 MiB\n", STRATA_ENGINE);
    let tracked = [on_sheep(
        "iq2_xs",
        Footprint {
            vram: Vram::All,
            ram: 37 * GIB,
        },
    )];
    let measures = measure(&Inputs {
        tracked: &tracked,
        flock: &flock,
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &BTreeMap::new(),
    });
    assert_eq!(
        measure_of("iq2_xs", &measures),
        Measured {
            vram: None,
            ram: Some(80 * MIB)
        }
    );
    assert_eq!(measures.unaccounted_vram, None);
}

/// laya is tracked; the sheep `trainer` runs no model, so its GPU process is unaccounted; the
/// ollama sheep runs no tracked model either, and its runner is owned through its blob.
#[test]
fn unaccounted_is_what_no_tracked_sheep_or_ollama_runner_owns() {
    let flock = [
        row("laya", 1_000, &[(1_001, "python3")], None),
        row("trainer", 5_000, &[(5_001, "python3")], None),
        row("ollama", 2_000, &[(QWEN_RUNNER_PID, "llama-server")], None),
    ];
    let apps = format!(
        "1001, /usr/bin/python3, 1500 MiB\n{QWEN_RUNNER_APP}5001, /usr/bin/python3, 2000 MiB\n"
    );
    let reading = gpu("23100 MiB, 24564 MiB\n", &apps);
    let tracked = [on_sheep("laya", laya_gpu())];
    let blobs = [QWEN_BLOB.to_owned()];
    let measures = measure(&Inputs {
        tracked: &tracked,
        flock: &flock,
        blobs: &blobs,
        gpu: Some(&reading),
        cmdlines: &runner(),
    });
    assert_eq!(
        measures.unaccounted_vram,
        Some(2_058 * MIB),
        "trainer's 2000 MiB and 58 MiB nobody runs"
    );
}

#[test]
fn without_nvidia_smi_nothing_is_measured_on_the_gpu() {
    let flock = [row("laya", 1_000, &[(1_001, "python3")], Some(1_504 * MIB))];
    let tracked = [on_sheep("laya", laya_gpu())];
    let measures = measure(&Inputs {
        tracked: &tracked,
        flock: &flock,
        blobs: &[],
        gpu: None,
        cmdlines: &BTreeMap::new(),
    });
    assert_eq!(
        measure_of("laya", &measures),
        Measured {
            vram: None,
            ram: Some(1_504 * MIB)
        }
    );
    assert_eq!(measures.unaccounted_vram, None);
}

#[test]
fn a_sheep_missing_from_the_flock_is_unmeasured() {
    let tracked = [on_sheep("laya", laya_gpu())];
    let reading = gpu(IDLE_TOTALS, "");
    let measures = measure(&Inputs {
        tracked: &tracked,
        flock: &[],
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &BTreeMap::new(),
    });
    assert_eq!(measure_of("laya", &measures), Measured::default());
}

// A command line can carry a key, so a lazy derive must fail here.
#[test]
fn inputs_debug_leaves_out_command_lines() {
    let inputs = Inputs {
        tracked: &[],
        flock: &[],
        blobs: &[],
        gpu: None,
        cmdlines: &runner(),
    };
    assert_eq!(
        format!("{inputs:?}"),
        "Inputs { tracked: [], flock: [], blobs: [], gpu: None, .. }"
    );
}

#[test]
fn ten_percent_over_is_not_drift_and_more_is() {
    let declared = Footprint {
        vram: Vram::Bytes(1_000 * MIB),
        ram: 1_000 * MIB,
    };
    assert!(!drifts(
        declared,
        Measured {
            vram: Some(1_100 * MIB),
            ram: Some(1_100 * MIB)
        }
    ));
    assert!(drifts(
        declared,
        Measured {
            vram: Some(1_100 * MIB + 1),
            ram: None
        }
    ));
    assert!(drifts(
        declared,
        Measured {
            vram: None,
            ram: Some(1_100 * MIB + 1)
        }
    ));
}

#[test]
fn a_figure_declared_all_or_left_unmeasured_never_drifts() {
    assert!(!drifts(
        Footprint {
            vram: Vram::All,
            ram: 37 * GIB
        },
        Measured {
            vram: Some(24 * GIB),
            ram: None
        }
    ));
    assert!(!drifts(
        Footprint {
            vram: Vram::Bytes(MIB),
            ram: MIB
        },
        Measured::default()
    ));
}

#[test]
fn vram_on_a_model_that_declares_none_is_drift() {
    let laya_in_ram = Footprint {
        vram: Vram::None,
        ram: 5 * GIB,
    };
    assert!(drifts(
        laya_in_ram,
        Measured {
            vram: Some(300 * MIB),
            ram: None
        }
    ));
}

#[test]
fn drift_is_logged_once_when_it_starts_and_once_when_it_stops() {
    let laya = ModelName::from("laya");
    let at = |vram_mib: u64| {
        let measured = Measured {
            vram: Some(vram_mib * MIB),
            ram: Some(1_504 * MIB),
        };
        BTreeMap::from([(laya.clone(), (laya_gpu(), measured))])
    };
    let mut drifting = Drifting::default();
    assert_eq!(drifting.update(&at(5_000)), Vec::<String>::new());
    assert_eq!(
        drifting.update(&at(7_000)),
        vec![
            "paddock: laya is drifting: it measures 7000 MiB VRAM and 1504 MiB RAM \
              against 6144 MiB VRAM and 2048 MiB RAM declared"
                .to_owned()
        ]
    );
    assert!(drifting.contains(&laya));
    assert_eq!(
        drifting.update(&at(7_100)),
        Vec::<String>::new(),
        "still drifting says nothing"
    );
    assert_eq!(
        drifting.update(&at(5_000)),
        vec!["paddock: laya is back within its declared footprint".to_owned()]
    );
    assert!(!drifting.contains(&laya));
}

#[test]
fn a_model_that_unloads_while_drifting_is_dropped_without_a_line() {
    let laya = ModelName::from("laya");
    let measured = Measured {
        vram: Some(7_000 * MIB),
        ram: None,
    };
    let mut drifting = Drifting::default();
    assert_eq!(
        drifting
            .update(&BTreeMap::from([(laya.clone(), (laya_gpu(), measured))]))
            .len(),
        1
    );
    assert_eq!(drifting.update(&BTreeMap::new()), Vec::<String>::new());
    assert!(!drifting.contains(&laya));
}
