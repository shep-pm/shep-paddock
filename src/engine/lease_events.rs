//! A lease's stream: what it hears while it waits may be dropped, what ends the wait never is.

use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use tokio::sync::mpsc;

use super::LeaseEvent;

// A lease's stream carries a few `Waiting`s in its life, one per change of
// reason. Its reader drains them as they come, so one this far behind is stuck.
const LEASE_EVENTS: usize = 32;

/// The engine's end of a lease's stream
#[derive(Debug, Clone)]
pub(crate) struct LeaseSender {
    waiting: mpsc::Sender<LeaseEvent>,
    rest: mpsc::UnboundedSender<LeaseEvent>,
}

/// The holder's end of a lease's stream
///
/// Dropping it closes the stream, which the engine reads as a hang-up.
#[derive(Debug)]
pub(crate) struct LeaseEvents {
    waiting: mpsc::Receiver<LeaseEvent>,
    rest: mpsc::UnboundedReceiver<LeaseEvent>,
}

/// A new lease stream
pub(crate) fn lease_channel() -> (LeaseSender, LeaseEvents) {
    let (waiting, waiting_rx) = mpsc::channel(LEASE_EVENTS);
    let (rest, rest_rx) = mpsc::unbounded_channel();
    (
        LeaseSender { waiting, rest },
        LeaseEvents {
            waiting: waiting_rx,
            rest: rest_rx,
        },
    )
}

impl LeaseSender {
    /// Sends `event`, unless its reader has gone
    ///
    /// A `Waiting` is dropped when the reader is [`LEASE_EVENTS`] behind, since
    /// a later one says the same. Every other event arrives.
    pub fn send(&self, event: LeaseEvent) {
        match event {
            LeaseEvent::Waiting { .. } => {
                let _ = self.waiting.try_send(event);
            }
            other => {
                let _ = self.rest.send(other);
            }
        }
    }

    /// Resolves once the reader has gone
    pub async fn closed(&self) {
        self.rest.closed().await;
    }

    /// Whether the reader has gone
    pub fn is_closed(&self) -> bool {
        self.rest.is_closed()
    }
}

impl LeaseEvents {
    /// The next event, or `None` once the engine has dropped the stream and it is drained
    ///
    /// # Cancellation safety
    /// Safe: an event is taken only when it is returned.
    pub async fn recv(&mut self) -> Option<LeaseEvent> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// As [`Self::recv`], for a caller that polls
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<LeaseEvent>> {
        // Every `Waiting` is sent before what ends the wait, so it is read first.
        if let Poll::Ready(Some(event)) = self.waiting.poll_recv(cx) {
            return Poll::Ready(Some(event));
        }
        self.rest.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use tokio::time::timeout;

    use super::*;
    use crate::book::{Ended, Reason};

    fn waiting(n: usize) -> LeaseEvent {
        LeaseEvent::Waiting {
            reason: Reason::Loading {
                model: format!("m{n}").into(),
            },
            estimate: None,
        }
    }

    async fn drained(events: &mut LeaseEvents) -> Vec<LeaseEvent> {
        let mut heard = Vec::new();
        while let Some(event) = timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("the stream ends once drained")
        {
            heard.push(event);
        }
        heard
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_arrives_past_a_full_stream_of_waits() {
        let (sender, mut events) = lease_channel();
        for n in 0..LEASE_EVENTS + 8 {
            sender.send(waiting(n));
        }
        sender.send(LeaseEvent::Ended(Ended::Released));
        drop(sender);

        let heard = drained(&mut events).await;

        let mut want: Vec<_> = (0..LEASE_EVENTS).map(waiting).collect();
        want.push(LeaseEvent::Ended(Ended::Released));
        assert_eq!(heard, want);
    }

    #[tokio::test(start_paused = true)]
    async fn waits_are_read_before_the_grant_sent_after_them() {
        let (sender, mut events) = lease_channel();
        let lease = crate::book::LeaseId(3);
        sender.send(waiting(0));
        sender.send(LeaseEvent::Granted { lease });
        sender.send(LeaseEvent::Ended(Ended::Expired));
        drop(sender);

        let heard = drained(&mut events).await;

        assert_eq!(
            heard,
            [
                waiting(0),
                LeaseEvent::Granted { lease },
                LeaseEvent::Ended(Ended::Expired)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_reader_closes_the_sender() {
        let (sender, events) = lease_channel();
        assert!(!sender.is_closed());
        drop(events);
        timeout(Duration::from_secs(1), sender.closed())
            .await
            .expect("closed once the reader is gone");
        assert!(sender.is_closed());
    }
}
