use super::*;

/// Asked leases by id, each with its model and whether it is reclaimable;
/// granted held leases with their model and whether its backend has exited
/// since the grant or its last reload; and granted reclaimable leases. Kept
/// from outside the book.
#[derive(Debug, Default)]
pub(super) struct Granted {
    asked: BTreeMap<LeaseId, (ModelName, bool)>,
    live: BTreeMap<LeaseId, (ModelName, bool)>,
    reclaimable: BTreeSet<LeaseId>,
}

impl Granted {
    pub(super) fn saw_event(&mut self, event: &Event) {
        match event {
            Event::LeaseAsked { ask, .. } => {
                if let Some(model) = ask.model() {
                    self.asked
                        .insert(ask.lease, (model.clone(), ask.reclaimable));
                }
            }
            Event::BackendExited { model } => {
                for (held, exited) in self.live.values_mut() {
                    *exited |= held == model;
                }
            }
            _ => {}
        }
    }

    pub(super) fn saw_actions(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::Grant { lease, .. } => match self.asked.get(lease) {
                    Some((_, true)) => {
                        self.reclaimable.insert(*lease);
                    }
                    Some((model, false)) => {
                        self.live.insert(*lease, (model.clone(), false));
                    }
                    None => {}
                },
                Action::LeaseEnded { lease, .. } => {
                    self.live.remove(lease);
                    self.reclaimable.remove(lease);
                }
                _ => {}
            }
        }
    }

    /// A held model that is not Loaded with no exit to explain it
    ///
    /// A held model is never evicted. It may be anything but Evicting while
    /// it loads again after an exit, and the exit is forgotten once it has.
    pub(super) fn broken(&mut self, book: &Book) -> Option<String> {
        let found = self.live.iter().find_map(|(lease, (model, exited))| {
            let state = book.state(model);
            let excused = *exited && state != Some(State::Evicting);
            (state != Some(State::Loaded) && !excused)
                .then(|| format!("{model} is {state:?} under lease {lease:?}"))
        });
        for (model, exited) in self.live.values_mut() {
            *exited &= book.state(model) != Some(State::Loaded);
        }
        found
    }

    /// A waiter told, or refused, because of a reclaimable lease
    pub(super) fn blocked_by_reclaimable(&self, actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|action| {
            let reason = match action {
                Action::Waiting { reason, .. } => reason,
                Action::Refuse { refusal, .. } => &refusal.reason,
                _ => return None,
            };
            match reason {
                Reason::Held { lease, .. } if self.is_reclaimable(lease) => {
                    Some(format!("{action:?} names reclaimable lease {lease:?}"))
                }
                Reason::Behind {
                    model: Taker::Model(model),
                } if self.only_reclaimable(model) => Some(format!(
                    "{action:?} waits behind {model}, which only reclaimable leases name"
                )),
                _ => None,
            }
        })
    }

    /// Whether `lease` was asked as reclaimable, so one granted this step counts too
    fn is_reclaimable(&self, lease: &LeaseId) -> bool {
        self.asked
            .get(lease)
            .is_some_and(|(_, reclaimable)| *reclaimable)
    }

    /// Whether live reclaimable leases name `model` and no held one does
    fn only_reclaimable(&self, model: &ModelName) -> bool {
        let named = |lease: &LeaseId| {
            self.asked
                .get(lease)
                .is_some_and(|(asked, _)| asked == model)
        };
        self.reclaimable.iter().any(named) && !self.live.values().any(|(held, _)| held == model)
    }
}
