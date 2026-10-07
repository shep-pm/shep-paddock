//! Which placement a model loads in.

use super::Book;
use crate::{
    config::{ModelName, PlacementName},
    footprint::Footprint,
};

impl Book {
    /// The placement `model` claimed room in, or is loading or loaded in, if it has one
    pub fn placement(&self, model: &ModelName) -> Option<PlacementName> {
        self.slots.get(model)?.placement.clone()
    }

    /// The ways `model` can load, in the order they are tried
    ///
    /// A model without placements has one: the figures it counts at now.
    pub(super) fn options(&self, model: &ModelName) -> Vec<(Option<PlacementName>, Footprint)> {
        match self.config.models.get(model) {
            Some(configured) if !configured.placements.is_empty() => configured
                .placements
                .iter()
                .map(|placement| (Some(placement.name.clone()), placement.footprint))
                .collect(),
            _ => self
                .slots
                .get(model)
                .map(|slot| vec![(None, self.counted(model, slot))])
                .unwrap_or_default(),
        }
    }

    /// Records the placement `model` claims room in, at the figures it declares
    pub(super) fn place(
        &mut self,
        model: &ModelName,
        placement: Option<PlacementName>,
        footprint: Footprint,
    ) {
        if let Some(slot) = self.slots.get_mut(model) {
            slot.placement = placement;
            slot.footprint = footprint;
        }
    }
}
