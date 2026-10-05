//! Who waits, and why.

use super::{Action, WaiterId};
use crate::config::ModelName;

/// Why a waiter cannot be served yet
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Its model is loading, or will load once the memory it needs is free.
    Loading {
        /// The waiter's model.
        model: ModelName,
    },
    /// Its model is being evicted to make room for another.
    Evicting {
        /// The waiter's model.
        model: ModelName,
        /// The model the room goes to.
        for_model: ModelName,
    },
    /// Its model is unloading for a reason other than an eviction.
    Draining {
        /// The waiter's model.
        model: ModelName,
    },
    /// No eviction can make room while another model holds or claims it.
    Behind {
        /// The model holding or claiming the room.
        model: ModelName,
    },
}

/// A request that cannot be served yet
#[derive(Debug)]
pub(super) struct Waiter {
    pub id: WaiterId,
    pub model: ModelName,
    told: Option<Reason>,
}

impl Waiter {
    pub fn new(id: WaiterId, model: ModelName) -> Self {
        Self {
            id,
            model,
            told: None,
        }
    }

    /// The `Waiting` action for `reason`, or `None` when it was the last one told
    pub fn tell(&mut self, reason: Reason) -> Option<Action> {
        if self.told.as_ref() == Some(&reason) {
            return None;
        }
        self.told = Some(reason.clone());
        Some(Action::Waiting {
            waiter: self.id,
            reason,
            estimate: None,
        })
    }
}
