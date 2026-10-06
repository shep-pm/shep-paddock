//! Attribution from output captured on the GPU host. Parsing and drift have their own files.

use std::collections::BTreeMap;

use shep_client::shep_core::{
    protocol::{Lamb, ProcessInfo},
    status::ProcStatus,
};

use super::{
    gpu::{GpuReading, reading},
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

mod drift;
mod gpu;

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

#[test]
fn an_ollama_model_whose_blob_ollama_does_not_list_is_only_unaccounted() {
    let reading = gpu("19600 MiB, 24564 MiB\n", QWEN_RUNNER_APP);
    let measures = measure(&Inputs {
        tracked: &[qwen(Some(QWEN_BLOB))],
        flock: &[],
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &runner(),
    });
    assert_eq!(measure_of("qwen3.8:27b", &measures).vram, None);
    assert_eq!(measures.unaccounted_vram, Some(19_600 * MIB));
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
