//! Lease streams: who hears each lease's events, and noticing a reader that left.

use futures_util::{FutureExt as _, future::abortable};

use super::{Engine, Watched};
use crate::{
    book::{Event, Hold, LeaseId, LeaseView, WaiterId},
    config::ClientName,
    engine::{LeaseEvent, LeaseRefused, LeaseSender},
};

impl Engine {
    pub(super) fn hold(&mut self, lease: LeaseId, events: LeaseSender) {
        self.watch(Watched::Holder(lease), events.clone());
        self.holders.insert(lease, events);
    }

    pub(super) fn watch(&mut self, watched: Watched, events: LeaseSender) {
        let (closed, handle) = abortable(async move {
            events.closed().await;
            watched
        });
        self.watching.entry(watched).or_default().push(handle);
        self.watchers.push(closed.map(Result::ok).boxed_local());
    }

    /// Drops the watchers on `watched`, and with them their senders
    pub(super) fn unwatch(&mut self, watched: Watched) {
        for handle in self.watching.remove(&watched).unwrap_or_default() {
            handle.abort();
        }
    }

    /// Sends a waiting lease its last event, after which its stream ends
    pub(super) fn end_waiting(&mut self, waiter: WaiterId, last: LeaseEvent) {
        if let Some(events) = self.waiting_leases.remove(&waiter) {
            self.unwatch(Watched::Waiter(waiter));
            events.send(last);
        }
    }

    /// Reads a dropped lease stream: a waiting lease leaves the queue, and a
    /// connection lease's holder detaches
    pub fn hung_up(&mut self, watched: Watched) {
        match watched {
            Watched::Waiter(waiter) => {
                if self
                    .waiting_leases
                    .get(&waiter)
                    .is_some_and(LeaseSender::is_closed)
                {
                    self.waiting_leases.remove(&waiter);
                    self.unwatch(Watched::Waiter(waiter));
                    self.feed(Event::WaiterGone { waiter });
                }
            }
            Watched::Holder(lease) => {
                if !self.holders.get(&lease).is_some_and(LeaseSender::is_closed) {
                    return;
                }
                self.holders.remove(&lease);
                self.unwatch(Watched::Holder(lease));
                let connection = self
                    .book
                    .lease(lease)
                    .is_some_and(|view| view.hold == Hold::Connection)
                    || self.book.awaits_detach(lease);
                if connection {
                    self.feed(Event::HolderDetached { lease });
                }
            }
        }
    }

    /// Whether `client` holds the granted lease, once leases past their end have ended
    pub(super) fn owned(
        &mut self,
        client: &ClientName,
        lease: LeaseId,
    ) -> Result<(), LeaseRefused> {
        self.feed(Event::Tick);
        match self.book.lease(lease) {
            None => Err(LeaseRefused::NotFound),
            Some(view) if view.client != *client => Err(LeaseRefused::NotYours),
            Some(_) => Ok(()),
        }
    }

    /// Revokes `lease` as the admin client `by`, logging who revoked which lease, its holder and why
    ///
    /// # Errors
    /// [`LeaseRefused::NotFound`] when no granted lease has that id, once leases past their end
    /// have ended, and [`LeaseRefused::Protected`] when a protected client other than `by` holds it.
    pub(super) fn revoke(
        &mut self,
        by: ClientName,
        lease: LeaseId,
        note: Option<String>,
    ) -> Result<(), LeaseRefused> {
        self.feed(Event::Tick);
        let view = self.book.lease(lease).ok_or(LeaseRefused::NotFound)?;
        let protected = self
            .config
            .clients
            .iter()
            .any(|client| client.name == view.client && client.protected);
        if protected && view.client != by {
            return Err(LeaseRefused::Protected);
        }
        eprintln!("{}", revoke_line(&by, &view, note.as_deref()));
        self.feed(Event::LeaseRevoked { lease, by, note });
        Ok(())
    }

    pub(super) fn attach(
        &mut self,
        client: &ClientName,
        lease: LeaseId,
        events: LeaseSender,
    ) -> Result<(), LeaseRefused> {
        self.owned(client, lease)?;
        if self
            .holders
            .get(&lease)
            .is_some_and(|open| !open.is_closed())
        {
            return Err(LeaseRefused::Attached);
        }
        events.send(LeaseEvent::Granted { lease });
        self.hold(lease, events);
        self.feed(Event::HolderAttached { lease });
        Ok(())
    }
}

/// The log line for `by` revoking `view`'s lease, its reason quoted so it cannot forge a line
fn revoke_line(by: &ClientName, view: &LeaseView, note: Option<&str>) -> String {
    let what = match (&view.model, view.footprint) {
        (Some(model), _) => format!(" on {model}"),
        (None, Some(footprint)) => format!(" ({footprint})"),
        (None, None) => String::new(),
    };
    let why = note.map_or_else(|| "no reason given".to_owned(), |note| format!("{note:?}"));
    format!(
        "paddock: {by} revoked lease {} of {}{what}: {why}",
        view.id, view.client
    )
}

#[cfg(test)]
mod tests {
    use super::revoke_line;
    use crate::{
        book::{Hold, LeaseId, LeaseView, Moment, Priority},
        config::{ClientName, ModelName},
        footprint::{Footprint, Vram},
    };

    fn view(model: Option<&str>, footprint: Option<Footprint>) -> LeaseView {
        LeaseView {
            id: LeaseId(12),
            client: ClientName::from("bench-01"),
            model: model.map(ModelName::from),
            footprint,
            pid: None,
            priority: Priority::Batch,
            since: Moment(0),
            expected_until: None,
            note: None,
            hold: Hold::Connection,
            attached: true,
            reclaimable: false,
            last_activity: Moment(0),
            in_use: false,
            release_if_idle: None,
            revoked: None,
        }
    }

    #[test]
    fn a_revoke_is_logged_with_who_which_lease_its_holder_and_why() {
        let mac = ClientName::from("mac-sessions");
        let on_model = view(Some("iq2_xs"), None);
        assert_eq!(
            revoke_line(&mac, &on_model, Some("forgotten since Tuesday")),
            r#"paddock: mac-sessions revoked lease L12 of bench-01 on iq2_xs: "forgotten since Tuesday""#
        );
        let bare = view(
            None,
            Some(Footprint {
                vram: Vram::Bytes(12 << 30),
                ram: 4 << 30,
            }),
        );
        assert_eq!(
            revoke_line(&mac, &bare, None),
            "paddock: mac-sessions revoked lease L12 of bench-01 (12G VRAM, 4G RAM): no reason given"
        );
        assert_eq!(
            revoke_line(&mac, &on_model, Some("a\nb")),
            r#"paddock: mac-sessions revoked lease L12 of bench-01 on iq2_xs: "a\nb""#,
            "a newline cannot forge a second line"
        );
    }
}
