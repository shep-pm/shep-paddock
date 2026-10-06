//! Lease streams: who hears each lease's events, and noticing a reader that left.

use futures_util::{FutureExt as _, future::abortable};
use tokio::sync::mpsc;

use super::{Engine, Watched};
use crate::{
    book::{Event, Hold, LeaseId, WaiterId},
    config::ClientName,
    engine::{LeaseEvent, LeaseRefused},
};

impl Engine {
    pub(super) fn hold(&mut self, lease: LeaseId, events: mpsc::Sender<LeaseEvent>) {
        self.watch(Watched::Holder(lease), events.clone());
        self.holders.insert(lease, events);
    }

    pub(super) fn watch(&mut self, watched: Watched, events: mpsc::Sender<LeaseEvent>) {
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
            let _ = events.try_send(last);
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
                    .is_some_and(mpsc::Sender::is_closed)
                {
                    self.waiting_leases.remove(&waiter);
                    self.unwatch(Watched::Waiter(waiter));
                    self.feed(Event::WaiterGone { waiter });
                }
            }
            Watched::Holder(lease) => {
                if !self
                    .holders
                    .get(&lease)
                    .is_some_and(mpsc::Sender::is_closed)
                {
                    return;
                }
                self.holders.remove(&lease);
                self.unwatch(Watched::Holder(lease));
                let connection = self
                    .book
                    .lease(lease)
                    .is_some_and(|view| view.hold == Hold::Connection);
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

    pub(super) fn attach(
        &mut self,
        client: &ClientName,
        lease: LeaseId,
        events: mpsc::Sender<LeaseEvent>,
    ) -> Result<(), LeaseRefused> {
        self.owned(client, lease)?;
        if self
            .holders
            .get(&lease)
            .is_some_and(|open| !open.is_closed())
        {
            return Err(LeaseRefused::Attached);
        }
        let _ = events.try_send(LeaseEvent::Granted { lease });
        self.hold(lease, events);
        self.feed(Event::HolderAttached { lease });
        Ok(())
    }
}
