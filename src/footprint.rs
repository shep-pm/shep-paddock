//! What one model costs the host while it is loaded.
//!
//! A footprint is the VRAM and RAM a model's backend holds. A [`Vram::All`]
//! footprint is a model that grows into whatever VRAM is free, so it counts as
//! the whole card and nothing else with VRAM fits beside it.

use core::fmt;

use shep_client::shep_core::values::MemSize;

/// The VRAM a model holds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vram {
    /// Declares no VRAM, such as a CPU-only model
    None,
    /// A fixed amount, in bytes
    Bytes(u64),
    /// Grows into whatever VRAM is free, so it counts as the host's whole VRAM
    All,
}

/// The VRAM and RAM a model holds while loaded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Footprint {
    /// VRAM held
    pub vram: Vram,
    /// RAM held, in bytes
    pub ram: u64,
}

impl Vram {
    /// The larger of the two, where `All` is larger than any byte count
    fn larger(self, other: Vram) -> Vram {
        match (self, other) {
            (Vram::All, _) | (_, Vram::All) => Vram::All,
            (Vram::Bytes(a), Vram::Bytes(b)) => Vram::Bytes(a.max(b)),
            (Vram::Bytes(n), Vram::None) | (Vram::None, Vram::Bytes(n)) => Vram::Bytes(n),
            (Vram::None, Vram::None) => Vram::None,
        }
    }
}

impl Footprint {
    /// The larger of the two in each resource
    pub fn larger(self, other: Footprint) -> Footprint {
        Footprint {
            vram: self.vram.larger(other.vram),
            ram: self.ram.max(other.ram),
        }
    }
}

/// `12G VRAM, 4G RAM` in shep's size grammar, with `no` and `all` for those VRAM declarations
impl fmt::Display for Footprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.vram {
            Vram::None => f.write_str("no")?,
            Vram::All => f.write_str("all")?,
            Vram::Bytes(bytes) => write!(f, "{}", MemSize::from_bytes(bytes))?,
        }
        write!(f, " VRAM, {} RAM", MemSize::from_bytes(self.ram))
    }
}

/// What the host has to lease
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Host {
    /// Total VRAM, in bytes.
    pub vram: u64,
    /// Total RAM, in bytes.
    pub ram: u64,
}

impl Host {
    /// Whether the footprints together fit, each resource within the host
    ///
    /// Sums saturate, so a pathological config cannot wrap into a fit.
    pub fn fits<'a>(&self, footprints: impl IntoIterator<Item = &'a Footprint>) -> bool {
        let (vram, ram) = self.sum(footprints);
        vram <= self.vram && ram <= self.ram
    }

    /// The footprints together, in bytes, as [`Host::fits`] counts them
    pub fn declared<'a>(&self, footprints: impl IntoIterator<Item = &'a Footprint>) -> Footprint {
        let (vram, ram) = self.sum(footprints);
        Footprint {
            vram: Vram::Bytes(vram),
            ram,
        }
    }

    fn sum<'a>(&self, footprints: impl IntoIterator<Item = &'a Footprint>) -> (u64, u64) {
        footprints
            .into_iter()
            .fold((0_u64, 0_u64), |(vram, ram), fp| {
                let held = match fp.vram {
                    Vram::None => 0,
                    Vram::Bytes(bytes) => bytes,
                    Vram::All => self.vram,
                };
                (vram.saturating_add(held), ram.saturating_add(fp.ram))
            })
    }

    /// Whether the footprint fits on an otherwise empty host
    pub fn ever_fits(&self, footprint: &Footprint) -> bool {
        self.fits([footprint])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn host() -> Host {
        Host {
            vram: 24 * GIB,
            ram: 62 * GIB,
        }
    }

    fn fp(vram: Vram, ram_gib: u64) -> Footprint {
        Footprint {
            vram,
            ram: ram_gib * GIB,
        }
    }

    #[test]
    fn strata_and_qwen_never_fit_together() {
        let strata = fp(Vram::All, 37);
        let qwen = fp(Vram::Bytes(22 * GIB), 4);
        assert!(!host().fits([&strata, &qwen]));
    }

    #[test]
    fn strata_fits_beside_a_model_that_declares_no_vram() {
        let strata = fp(Vram::All, 37);
        let laya = fp(Vram::None, 5);
        assert!(host().fits([&strata, &laya]));
    }

    #[test]
    fn ram_alone_can_refuse() {
        let iq3_s = fp(Vram::All, 55);
        let big = fp(Vram::None, 8);
        assert!(!host().fits([&iq3_s, &big]));
    }

    #[test]
    fn nothing_loaded_fits() {
        assert!(host().fits([]));
    }

    #[test]
    fn exactly_full_fits() {
        assert!(host().fits([&fp(Vram::Bytes(24 * GIB), 62)]));
    }

    #[test]
    fn declared_counts_all_as_the_whole_card() {
        let strata = fp(Vram::All, 37);
        let laya = fp(Vram::None, 5);
        let qwen = fp(Vram::Bytes(22 * GIB), 4);
        assert_eq!(
            host().declared([&strata, &laya, &qwen]),
            fp(Vram::Bytes(46 * GIB), 46)
        );
    }

    #[test]
    fn larger_takes_each_resource_on_its_own() {
        let gpu = fp(Vram::Bytes(12 * GIB), 1);
        let cpu = fp(Vram::None, 8);
        assert_eq!(gpu.larger(cpu), fp(Vram::Bytes(12 * GIB), 8));
        assert_eq!(cpu.larger(gpu), fp(Vram::Bytes(12 * GIB), 8));
        assert_eq!(gpu.larger(fp(Vram::All, 0)), fp(Vram::All, 1));
        assert_eq!(
            gpu.larger(fp(Vram::Bytes(10 * GIB), 0)),
            fp(Vram::Bytes(12 * GIB), 1)
        );
        assert_eq!(cpu.larger(cpu), cpu);
    }

    #[test]
    fn a_model_bigger_than_the_host_never_fits() {
        assert!(!host().ever_fits(&fp(Vram::None, 63)));
    }

    #[test]
    fn a_footprint_reads_in_shep_size_grammar() {
        assert_eq!(fp(Vram::Bytes(12 * GIB), 4).to_string(), "12G VRAM, 4G RAM");
        assert_eq!(fp(Vram::None, 5).to_string(), "no VRAM, 5G RAM");
        assert_eq!(fp(Vram::All, 0).to_string(), "all VRAM, 0 RAM");
        let odd = Footprint {
            vram: Vram::Bytes(22_323 << 20),
            ram: 1_536 << 10,
        };
        assert_eq!(odd.to_string(), "22323M VRAM, 1536K RAM");
    }

    #[test]
    fn sums_saturate_instead_of_wrapping_into_a_fit() {
        let huge = fp(Vram::Bytes(u64::MAX), 0);
        let one = fp(Vram::Bytes(1), 0);
        assert!(!host().fits([&huge, &one]));
        assert!(!host().ever_fits(&huge));
        assert_eq!(host().declared([&huge, &one]), fp(Vram::Bytes(u64::MAX), 0));
        let ram = Footprint {
            vram: Vram::None,
            ram: u64::MAX,
        };
        assert!(!host().fits([&ram, &ram]));
    }
}
