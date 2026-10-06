//! Drift's threshold, and its log lines crossing both ways.

use std::collections::BTreeMap;

use super::{GIB, MIB, laya_gpu};
use crate::{
    config::ModelName,
    footprint::{Footprint, Vram},
    survey::{
        Measured,
        drift::{Drifting, Read, drifts},
    },
};

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
fn ten_percent_holds_on_figures_of_a_few_bytes() {
    let declared = Footprint {
        vram: Vram::Bytes(9),
        ram: 10,
    };
    assert!(
        drifts(
            declared,
            Measured {
                vram: Some(10),
                ram: None
            }
        ),
        "11% over"
    );
    assert!(
        !drifts(
            declared,
            Measured {
                vram: Some(9),
                ram: Some(11)
            }
        ),
        "exactly 10% over"
    );
    let nothing = Footprint {
        vram: Vram::None,
        ram: 0,
    };
    assert!(!drifts(
        nothing,
        Measured {
            vram: Some(0),
            ram: Some(0)
        }
    ));
    assert!(drifts(
        nothing,
        Measured {
            vram: None,
            ram: Some(1)
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
        BTreeMap::from([(laya.clone(), (laya_gpu(), measured, Read::BOTH))])
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
            .update(&BTreeMap::from([(
                laya.clone(),
                (laya_gpu(), measured, Read::BOTH)
            )]))
            .len(),
        1
    );
    assert_eq!(drifting.update(&BTreeMap::new()), Vec::<String>::new());
    assert!(!drifting.contains(&laya));
}

#[test]
fn a_figure_left_unread_keeps_its_drift_and_a_read_one_still_counts() {
    let laya = ModelName::from("laya");
    let at = |vram_mib: Option<u64>, ram_mib: u64, read: Read| {
        let measured = Measured {
            vram: vram_mib.map(|mib| mib * MIB),
            ram: Some(ram_mib * MIB),
        };
        BTreeMap::from([(laya.clone(), (laya_gpu(), measured, read))])
    };
    let vram_unread = Read {
        vram: false,
        ram: true,
    };
    let mut drifting = Drifting::default();
    assert_eq!(
        drifting.update(&at(Some(7_000), 1_504, Read::BOTH)).len(),
        1
    );

    assert_eq!(
        drifting.update(&at(None, 1_504, vram_unread)),
        Vec::<String>::new()
    );
    assert!(
        drifting.contains(&laya),
        "the VRAM it drifted on was not read"
    );

    assert_eq!(
        drifting.update(&at(Some(5_000), 1_504, Read::BOTH)).len(),
        1
    );
    assert_eq!(
        drifting.update(&at(None, 3_000, vram_unread)).len(),
        1,
        "RAM read over"
    );
    assert_eq!(
        drifting.update(&at(None, 1_504, vram_unread)).len(),
        1,
        "and back"
    );
    assert!(!drifting.contains(&laya));
}
