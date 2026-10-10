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
    /// Asked bare leases: what each declares, and whether a connection holds it.
    bare_asked: BTreeMap<LeaseId, (Footprint, bool)>,
    /// Granted bare leases.
    bare_live: BTreeMap<LeaseId, Footprint>,
    /// Granted bare leases whose connection holder is attached.
    attached: BTreeSet<LeaseId>,
    /// Revoked bare leases whose holder was attached, until it detaches.
    revoked_held: BTreeMap<LeaseId, Footprint>,
}

impl Granted {
    pub(super) fn saw_event(&mut self, event: &Event) {
        match event {
            Event::LeaseAsked { ask, .. } => match (ask.model(), ask.bare()) {
                (Some(model), _) => {
                    self.asked
                        .insert(ask.lease, (model.clone(), ask.reclaimable));
                }
                (None, Some(footprint)) => {
                    let connection = ask.hold == Hold::Connection;
                    self.bare_asked.insert(ask.lease, (footprint, connection));
                }
                (None, None) => {}
            },
            Event::BackendExited { model } => {
                for (held, exited) in self.live.values_mut() {
                    *exited |= held == model;
                }
            }
            Event::HolderDetached { lease } => {
                self.attached.remove(lease);
                self.revoked_held.remove(lease);
            }
            Event::HolderAttached { lease } => {
                let connection = self
                    .bare_asked
                    .get(lease)
                    .is_some_and(|(_, connection)| *connection);
                if connection && self.bare_live.contains_key(lease) {
                    self.attached.insert(*lease);
                }
            }
            _ => {}
        }
    }

    pub(super) fn saw_actions(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::Grant { lease, .. } => {
                    if let Some((footprint, connection)) = self.bare_asked.get(lease) {
                        self.bare_live.insert(*lease, *footprint);
                        if *connection {
                            self.attached.insert(*lease);
                        }
                        continue;
                    }
                    match self.asked.get(lease) {
                        Some((_, true)) => {
                            self.reclaimable.insert(*lease);
                        }
                        Some((model, false)) => {
                            self.live.insert(*lease, (model.clone(), false));
                        }
                        None => {}
                    }
                }
                Action::LeaseEnded { lease, why } => {
                    self.live.remove(lease);
                    self.reclaimable.remove(lease);
                    if let Some(footprint) = self.bare_live.remove(lease)
                        && matches!(why, Ended::Revoked(_))
                        && self.attached.contains(lease)
                    {
                        self.revoked_held.insert(*lease, footprint);
                    }
                    self.attached.remove(lease);
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

    /// What the book counts for bare leases, when it differs from each one granted and each
    /// revoked one whose holder has not detached
    pub(super) fn bare_miscounted(&self, book: &Book) -> Option<String> {
        let granted = book
            .leases
            .iter()
            .filter_map(|(id, lease)| Some((*id, lease.ask.bare()?)));
        let revoked = book
            .revoked
            .iter()
            .filter(|(_, revoked)| revoked.counted)
            .filter_map(|(id, revoked)| Some((*id, revoked.lease.ask.bare()?)));
        let counted: BTreeMap<LeaseId, Footprint> = granted.chain(revoked).collect();
        let expected: BTreeMap<LeaseId, Footprint> = self
            .bare_live
            .iter()
            .chain(&self.revoked_held)
            .map(|(id, footprint)| (*id, *footprint))
            .collect();
        (counted != expected)
            .then(|| format!("the book counts {counted:?} for bare leases, not {expected:?}"))
    }

    /// A granted bare lease ended as an evicted model's reclaimable lease would
    pub(super) fn bare_evicted(&self, actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|action| match action {
            Action::LeaseEnded {
                lease,
                why: Ended::Reclaimed,
            } if self.bare_live.contains_key(lease) => {
                Some(format!("bare lease {lease:?} was taken back"))
            }
            _ => None,
        })
    }
}
