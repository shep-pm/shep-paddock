//! What one model costs the host while it is loaded.
//!
//! A footprint is the VRAM and RAM a model's backend holds. A [`Vram::All`]
//! footprint is a model that grows into whatever VRAM is free, so it counts as
//! the whole card and nothing else with VRAM fits beside it.

/// The VRAM a model holds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vram {
    /// Declares no VRAM, such as a CPU-only model.
    None,
    /// A fixed amount, in bytes.
    Bytes(u64),
    /// Grows into whatever VRAM is free, so it counts as the host's whole VRAM.
    All,
}

/// The VRAM and RAM a model holds while loaded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Footprint {
    /// VRAM held.
    pub vram: Vram,
    /// RAM held, in bytes.
    pub ram: u64,
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
        let (vram, ram) = footprints
            .into_iter()
            .fold((0_u64, 0_u64), |(vram, ram), fp| {
                let held = match fp.vram {
                    Vram::None => 0,
                    Vram::Bytes(bytes) => bytes,
                    Vram::All => self.vram,
                };
                (vram.saturating_add(held), ram.saturating_add(fp.ram))
            });
        vram <= self.vram && ram <= self.ram
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
    fn a_model_bigger_than_the_host_never_fits() {
        assert!(!host().ever_fits(&fp(Vram::None, 63)));
    }
}
