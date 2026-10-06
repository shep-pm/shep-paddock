//! One shepherd subscription shared by everyone who wants an event from it.
//!
//! A connection carries one subscription, and a second `subscribe` on it replaces the first's
//! topics. The engine's process events and the config watcher's changes therefore ride one
//! subscription made here, and a task reads it and hands each event to whoever wants it.

use core::future::Future;
use futures_util::{Stream, StreamExt as _, stream, stream::LocalBoxStream};
use shep_client::{Lagged, shep_core::protocol::BusEvent};
use tokio::{
    sync::{Mutex, broadcast},
    task::spawn_local,
};

use super::{ProcessEvent, ShepherdError, process_event};

/// Events a slow consumer may fall behind by before it is told it lagged
pub(super) const CAPACITY: usize = 256;

/// What the shared subscription saw
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fan {
    /// A sheep's lifecycle event.
    Process(ProcessEvent),
    /// The named dog's section may have changed.
    Config(String),
    /// Events were dropped, and any of them may have mattered.
    Lagged,
}

/// The one live subscription, if there is one
#[derive(Default)]
pub(super) struct Hub {
    session: Mutex<Option<Session>>,
}

struct Session {
    // Weak, so the pump's sender is the last one and its end closes every consumer.
    events: broadcast::WeakSender<Fan>,
}

impl Hub {
    /// A receiver on the live subscription, opening one with `open` when there is none
    ///
    /// A subscription whose stream ended is replaced by the next call, so a consumer whose stream
    /// ended asks again and every consumer ends up on the same new one. Must run inside a
    /// `LocalSet`.
    ///
    /// # Errors
    /// Whatever `open` fails with.
    ///
    /// # Panics
    /// Outside a `LocalSet`, when it has to open a subscription.
    pub(super) async fn join<S>(
        &self,
        open: impl Future<Output = Result<S, ShepherdError>>,
    ) -> Result<broadcast::Receiver<Fan>, ShepherdError>
    where
        S: Stream<Item = Result<BusEvent, Lagged>> + Unpin + 'static,
    {
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref()
            && let Some(events) = session.events.upgrade()
        {
            return Ok(events.subscribe());
        }
        let mut source = open.await?;
        let (sender, receiver) = broadcast::channel(CAPACITY);
        let events = sender.downgrade();
        spawn_local(async move {
            while let Some(item) = source.next().await {
                let fan = match item {
                    Err(Lagged { .. }) => Fan::Lagged,
                    Ok(BusEvent::DogConfigChanged { dog }) => Fan::Config(dog),
                    Ok(other) => match process_event(Ok(other)) {
                        Some(event) => Fan::Process(event),
                        None => continue,
                    },
                };
                // No receiver is not an error: nobody is listening for now.
                let _ = sender.send(fan);
            }
        });
        *slot = Some(Session { events });
        Ok(receiver)
    }
}

/// What a consumer does with one event
pub(super) enum Pick<T> {
    /// Hand this item on.
    Keep(T),
    /// Not for this consumer.
    Skip,
    /// End the consumer's stream.
    End,
}

/// The items of `receiver` that `pick` keeps, ending when the subscription does
///
/// A consumer that fell behind is handed [`Fan::Lagged`], as if the shepherd had said so.
pub(super) fn consume<T: 'static>(
    receiver: broadcast::Receiver<Fan>,
    pick: impl Fn(Fan) -> Pick<T> + 'static,
) -> LocalBoxStream<'static, T> {
    stream::unfold((receiver, pick), |(mut receiver, pick)| async move {
        loop {
            let fan = match receiver.recv().await {
                Ok(fan) => fan,
                Err(broadcast::error::RecvError::Lagged(_)) => Fan::Lagged,
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            match pick(fan) {
                Pick::Keep(item) => return Some((item, (receiver, pick))),
                Pick::Skip => {}
                Pick::End => return None,
            }
        }
    })
    .boxed_local()
}

#[cfg(test)]
mod tests {
    use core::{cell::Cell, time::Duration};
    use std::rc::Rc;

    use futures_util::stream;
    use shep_client::shep_core::{
        protocol::{ProcessEventKind, ProcessInfo},
        status::ProcStatus,
    };
    use tokio::{task::LocalSet, time::timeout};

    use super::*;
    use crate::shepherd::{ProcessKind, config_pick, process_pick};

    /// Bounds every await, so a consumer that never hears anything fails instead of hanging.
    async fn within<T>(step: impl Future<Output = T>) -> T {
        timeout(Duration::from_secs(5), step)
            .await
            .expect("the step finished in time")
    }

    fn exit() -> Result<BusEvent, Lagged> {
        Ok(BusEvent::Process {
            event: ProcessEventKind::Exit,
            info: ProcessInfo::builder(1, "iq3_s", ProcStatus::Stopped).build(),
            manually: false,
            at_ms: 0,
        })
    }

    fn changed(dog: &str) -> Result<BusEvent, Lagged> {
        Ok(BusEvent::DogConfigChanged {
            dog: dog.to_owned(),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn one_subscription_serves_a_process_event_and_a_config_event() {
        let opens = Rc::new(Cell::new(0));
        let hub = Hub::default();
        LocalSet::new()
            .run_until(async {
                let open = || async {
                    opens.set(opens.get() + 1);
                    let items = vec![exit(), changed("other"), changed("paddock")];
                    Ok(stream::iter(items).chain(stream::pending()))
                };
                let mut processes = consume(hub.join(open()).await.expect("joins"), process_pick);
                let mut configs = consume(
                    hub.join(open()).await.expect("joins"),
                    config_pick("paddock".to_owned()),
                );

                let event = within(processes.next()).await.expect("a process event");
                assert_eq!(event.kind, ProcessKind::Exit);
                // One item, for the dog's own name: the other dog's change was filtered out.
                assert_eq!(within(configs.next()).await, Some(()));
                assert!(
                    timeout(Duration::from_secs(1), configs.next())
                        .await
                        .is_err(),
                    "a second config item arrived"
                );
            })
            .await;
        assert_eq!(opens.get(), 1, "the second consumer opened its own");
    }

    #[tokio::test(start_paused = true)]
    async fn when_the_stream_ends_every_consumer_ends_and_the_next_join_opens_again() {
        let opens = Rc::new(Cell::new(0));
        let hub = Hub::default();
        LocalSet::new()
            .run_until(async {
                let open = || async {
                    opens.set(opens.get() + 1);
                    Ok(stream::iter(vec![exit()]))
                };
                let mut processes = consume(hub.join(open()).await.expect("joins"), process_pick);
                let mut configs = consume(
                    hub.join(open()).await.expect("joins"),
                    config_pick("paddock".to_owned()),
                );

                assert!(within(processes.next()).await.is_some());
                assert!(within(processes.next()).await.is_none());
                assert!(within(configs.next()).await.is_none());

                let _again = within(hub.join(open())).await.expect("joins");
            })
            .await;
        assert_eq!(opens.get(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_consumer_pushed_past_the_capacity_ends_its_process_stream() {
        let hub = Hub::default();
        LocalSet::new()
            .run_until(async {
                let burst = (0..CAPACITY + 10).map(|_| exit()).collect::<Vec<_>>();
                let open = async { Ok(stream::iter(burst).chain(stream::pending())) };
                let mut processes = consume(hub.join(open).await.expect("joins"), process_pick);

                // The pump ran while nobody was reading, and an exit was dropped.
                assert!(within(processes.next()).await.is_none());
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_lag_the_shepherd_reports_ends_the_process_stream_and_counts_as_a_config_change() {
        let hub = Hub::default();
        LocalSet::new()
            .run_until(async {
                let open = async {
                    Ok(stream::iter(vec![Err(Lagged { count: 3 })]).chain(stream::pending()))
                };
                let receiver = hub.join(open).await.expect("joins");
                let mut processes = consume(receiver.resubscribe(), process_pick);
                let mut configs = consume(receiver, config_pick("paddock".to_owned()));

                assert!(within(processes.next()).await.is_none());
                assert_eq!(within(configs.next()).await, Some(()));
            })
            .await;
    }
}
