//! A model in a podman container, measured with what its container holds.

use super::*;
use crate::test_support::built::{CLIENT, ENGINE_APP, contained};

fn strata(container: Option<ContainerRead>) -> Tracked {
    Tracked {
        container,
        ..on_sheep(
            "iq3_xxs",
            Footprint {
                vram: Vram::All,
                ram: 55 * GIB,
            },
        )
    }
}

fn measured(tracked: Tracked) -> Measured {
    let flock = [row("iq3_xxs", CLIENT, &[], Some(106 * MIB))];
    let reading = gpu("23900 MiB, 24564 MiB\n", ENGINE_APP);
    let measures = measure(&Inputs {
        tracked: &[tracked],
        flock: &flock,
        blobs: &[],
        gpu: Some(&reading),
        cmdlines: &BTreeMap::new(),
        bare: &[],
        parents: &BTreeMap::new(),
    });
    measure_of("iq3_xxs", &measures)
}

#[test]
fn a_model_in_a_container_is_measured_with_what_the_container_holds() {
    assert_eq!(
        measured(strata(Some(contained()))),
        Measured {
            vram: Some(23_800 * MIB),
            ram: Some(106 * MIB + 2 * MIB + 53 * GIB)
        }
    );
}

#[test]
fn without_its_container_the_model_is_measured_by_its_sheep_alone() {
    assert_eq!(
        measured(strata(None)),
        Measured {
            vram: None,
            ram: Some(106 * MIB)
        }
    );
}
