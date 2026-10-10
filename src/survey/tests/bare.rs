//! A bare job's GPU memory and what it does to unaccounted.

use super::*;
use crate::{
    book::LeaseId,
    test_support::built::{BARE_APPS, bare_parents},
};

fn job(pid: Option<u32>, vram: Vram) -> BareJob {
    BareJob {
        lease: LeaseId(1),
        pid,
        declared: Footprint { vram, ram: GIB },
    }
}

fn measured(jobs: &[BareJob], parents: &BTreeMap<u32, u32>) -> Measures {
    let reading = gpu("9000 MiB, 24564 MiB\n", BARE_APPS);
    measure(&Inputs {
        tracked: &[],
        flock: &[],
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &BTreeMap::new(),
        bare: jobs,
        parents,
    })
}

#[test]
fn a_bare_jobs_vram_is_its_pid_and_every_process_below_it() {
    let measures = measured(&[job(Some(4321), Vram::Bytes(8 * GIB))], &bare_parents());
    assert_eq!(
        measures.leases[&LeaseId(1)],
        Measured {
            vram: Some(7_000 * MIB),
            ram: None
        }
    );
    assert_eq!(measures.unaccounted_vram, Some(2_000 * MIB));
}

#[test]
fn a_bare_job_with_no_gpu_process_below_it_is_unmeasured() {
    let measures = measured(&[job(Some(9999), Vram::Bytes(8 * GIB))], &bare_parents());
    assert_eq!(measures.leases[&LeaseId(1)], Measured::default());
}

#[test]
fn a_bare_lease_without_a_pid_has_its_declared_vram_taken_off_unaccounted_down_to_zero() {
    let measures = measured(&[job(None, Vram::Bytes(4_096 * MIB))], &bare_parents());
    assert_eq!(measures.unaccounted_vram, Some(9_000 * MIB - 4_096 * MIB));
    let measures = measured(&[job(None, Vram::Bytes(20 * GIB))], &bare_parents());
    assert_eq!(measures.unaccounted_vram, Some(0));
}

#[test]
fn a_bare_lease_declaring_all_leaves_unaccounted_absent() {
    let measures = measured(&[job(Some(4321), Vram::All)], &bare_parents());
    assert_eq!(measures.unaccounted_vram, None);
}

#[test]
fn a_parent_chain_that_loops_reaches_no_lease() {
    let looping = BTreeMap::from([(5001, 5002), (5002, 5001), (7000, 1)]);
    let measures = measured(&[job(Some(4321), Vram::Bytes(8 * GIB))], &looping);
    assert_eq!(measures.leases[&LeaseId(1)], Measured::default());
}

/// Every process descends from init, so a lease naming it would own the whole GPU.
#[test]
fn a_lease_naming_pid_1_or_0_counts_as_having_no_pid() {
    for pid in [0, 1] {
        let measures = measured(&[job(Some(pid), Vram::Bytes(4_096 * MIB))], &bare_parents());
        assert_eq!(measures.leases[&LeaseId(1)], Measured::default());
        assert_eq!(measures.unaccounted_vram, Some(9_000 * MIB - 4_096 * MIB));
    }
}
