//! Meter readings: splitting each one's energy among the leases in use.

use std::collections::BTreeMap;

use tokio::time::Instant;

use super::Engine;
use crate::{
    book::LeaseId,
    config::ModelName,
    engine::{Metered, Share, Use, Used},
    meter::Source,
};

const JOULES_PER_WH: f64 = 3600.0;
const JOULES_PER_KWH: f64 = 3_600_000.0;

/// The last reading
#[derive(Debug, Clone, Copy)]
pub(super) struct Power {
    at: Instant,
    source: Source,
    priced: bool,
}

/// What one lease has used since the engine started
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Spent {
    joules: f64,
    usd: f64,
}

impl Engine {
    /// Adds the energy of the span up to `metered` to the leases in use, or to idle with none
    ///
    /// The first reading spans nothing. Returns where the energy went.
    pub fn metered(&mut self, metered: Metered) -> Vec<Share> {
        let span = self.power.map_or(0.0, |last| {
            metered.at.saturating_duration_since(last.at).as_secs_f64()
        });
        self.power = Some(Power {
            at: metered.at,
            source: metered.reading.source,
            priced: metered.usd_per_kwh.is_some(),
        });
        let joules = metered.reading.watts * span;
        let using: Vec<(LeaseId, ModelName)> = self
            .book
            .leases()
            .into_iter()
            .filter(|view| view.in_use)
            .filter_map(|view| Some((view.id, view.model?)))
            .collect();
        if using.is_empty() {
            return vec![Share {
                model: None,
                used: Use::Idle,
                joules,
            }];
        }
        let each = joules / using.len() as f64;
        let mut by_model: BTreeMap<ModelName, f64> = BTreeMap::new();
        for (lease, model) in using {
            let spent = self.energy.entry(lease).or_default();
            spent.joules += each;
            if let Some(price) = metered.usd_per_kwh {
                spent.usd += each / JOULES_PER_KWH * price;
            }
            *by_model.entry(model).or_default() += each;
        }
        by_model
            .into_iter()
            .map(|(model, joules)| Share {
                model: Some(model),
                used: Use::Lease,
                joules,
            })
            .collect()
    }

    /// What `lease` has used, or `None` while no meter has been read
    pub(super) fn used(&self, lease: LeaseId) -> Option<Used> {
        let power = self.power?;
        let spent = self.energy.get(&lease).copied().unwrap_or_default();
        Some(Used {
            wh: spent.joules / JOULES_PER_WH,
            usd: power.priced.then_some(spent.usd),
            source: power.source,
        })
    }

    /// What `lease` has used so far in milliwatt-hours, or `None` while no meter has been read
    pub(super) fn energy_mwh(&self, lease: LeaseId) -> Option<u64> {
        let used = self.used(lease)?;
        Some((used.wh * 1000.0).round() as u64)
    }

    /// Drops the energy of each lease the book no longer holds
    pub(super) fn forget_ended_energy(&mut self) {
        if self.energy.is_empty() {
            return;
        }
        let book = &self.book;
        self.energy.retain(|lease, _| book.lease(*lease).is_some());
    }
}
