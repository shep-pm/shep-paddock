//! Revoking a lease, and the revoked bare leases whose job may still run.

use super::{
    Action, Book, Ended, Hold, LeaseId, LeaseView, Moment,
    lease::{Lease, Revocation},
};

/// A revoked bare lease, listed while its job may still run
#[derive(Debug)]
pub(super) struct Revoked {
    pub(super) lease: Lease,
    pub(super) by: Revocation,
    /// Its holder was attached when it was revoked, so its memory stays counted until it detaches.
    pub(super) counted: bool,
}

impl Book {
    /// Ends lease `id`, revoked as `by` says
    ///
    /// A bare lease whose holder is attached on a connection keeps its memory
    /// counted until the holder detaches, since its job may still run. Any other
    /// bare lease frees its memory now, and is listed until its hold would have
    /// ended. A model lease just ends, and its model stays loaded.
    pub(super) fn revoke(&mut self, id: LeaseId, by: Revocation, out: &mut Vec<Action>) {
        let Some(lease) = self.leases.remove(&id) else {
            return;
        };
        out.push(Action::LeaseEnded {
            lease: id,
            why: Ended::Revoked(by.clone()),
        });
        out.push(Action::Persist);
        if lease.ask.bare().is_some() {
            let counted = lease.ask.hold == Hold::Connection && lease.attached();
            self.revoked.insert(id, Revoked { lease, by, counted });
        }
    }

    /// Forgets each revoked bare lease that holds no memory and whose hold would have ended by `now`
    pub(super) fn expire_revoked(&mut self, now: Moment) {
        let reconnect = self.config.reconnect;
        self.revoked.retain(|_, revoked| {
            revoked.counted || revoked.lease.ends_at(reconnect).is_some_and(|at| at > now)
        });
    }

    /// When each revoked bare lease that holds no memory stops being listed
    pub(super) fn revoked_ends(&self) -> impl Iterator<Item = Moment> + '_ {
        self.revoked
            .values()
            .filter(|revoked| !revoked.counted)
            .filter_map(|revoked| revoked.lease.ends_at(self.config.reconnect))
    }

    /// Each revoked bare lease the status still lists, marked revoked
    pub(super) fn revoked_views(&self) -> impl Iterator<Item = LeaseView> + '_ {
        self.revoked.values().map(|revoked| LeaseView {
            revoked: Some(revoked.by.clone()),
            ..revoked.lease.view(false)
        })
    }
}
