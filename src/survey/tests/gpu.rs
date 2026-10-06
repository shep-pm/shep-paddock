//! What `nvidia-smi` printed on the GPU host, read, and lines it could print that do not read.

use super::{GpuReading, MIB, gpu};
use crate::{
    survey::gpu::{GpuApp, GpuParseError, reading},
    test_support::captured::{IDLE_TOTALS, QWEN_RUNNER_APP, STRATA_ENGINE},
};

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
