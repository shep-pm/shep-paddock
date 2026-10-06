//! Checking the flock for a crash the process events missed while the subscription was down.

use std::collections::HashSet;

use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

use super::Engine;
use crate::{
    book::{Event, State},
    config::ModelName,
};

/// A sheep the engine expects to be running, and the load that put its model there
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::engine) struct Running {
    sheep: String,
    model: ModelName,
    load: u64,
}

impl Engine {
    /// Forgets the stop marks on sheep no unload is running for
    ///
    /// Stopping a sheep shep already held stopped publishes no `Stop`, so its
    /// mark would otherwise hide every later crash of that sheep.
    pub fn drop_stale_marks(&mut self, stopping: &HashSet<String>) {
        self.stopping.retain(|sheep| stopping.contains(sheep));
    }

    /// The sheep whose model is Loaded or Evicting, for a flock listing to check
    ///
    /// A Loading model is left out: a listing taken while its restart is under
    /// way shows the sheep stopped, and its load timeout bounds it anyway.
    pub fn expected_running(&self) -> Vec<Running> {
        self.on_sheep
            .iter()
            .filter(|(sheep, _)| !self.stopping.contains(*sheep))
            .filter(|(_, model)| {
                matches!(
                    self.book.state(model),
                    Some(State::Loaded | State::Evicting)
                )
            })
            .map(|(sheep, model)| Running {
                sheep: sheep.clone(),
                model: model.clone(),
                load: self.loads.get(model).copied().unwrap_or_default(),
            })
            .collect()
    }

    /// Tells the book of each expected sheep the flock shows not running
    ///
    /// Only a sheep still serving the same load, and not being stopped by the
    /// engine, counts: anything that changed since `expected` was taken was
    /// seen through events.
    pub fn reconcile(&mut self, expected: Vec<Running>, flock: &[ProcessInfo]) {
        for running in expected {
            let up = flock.iter().any(|row| {
                row.name == running.sheep
                    && matches!(row.status, ProcStatus::Starting | ProcStatus::Online)
            });
            let same_load = self.on_sheep.get(&running.sheep) == Some(&running.model)
                && self.loads.get(&running.model) == Some(&running.load);
            let serving = matches!(
                self.book.state(&running.model),
                Some(State::Loaded | State::Evicting)
            );
            if up || !same_load || !serving || self.stopping.contains(&running.sheep) {
                continue;
            }
            eprintln!(
                "paddock: sheep {} serving {} is not running in the flock",
                running.sheep, running.model
            );
            self.feed(Event::BackendExited {
                model: running.model,
            });
        }
    }
}
