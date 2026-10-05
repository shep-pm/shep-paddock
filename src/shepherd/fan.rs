//! One shepherd subscription shared by everyone who wants an event from it.
//!
//! A connection carries one subscription, and a second `subscribe` on it replaces the first's
//! topics. The engine's process events and the config watcher's changes therefore ride one
//! subscription made here, and a task reads it and hands each event to whoever wants it.

use core::future::Future;
use std::{cell::Cell, rc::Rc};

use futures_util::{Stream, StreamExt as _, stream, stream::LocalBoxStream};
use shep_client::{Lagged, shep_core::protocol::BusEvent};
use tokio::{
    sync::{Mutex, broadcast},
    task::spawn_local,
};

use super::{ProcessEvent, ShepherdError, process_event};

/// Events a slow consumer may fall behind by before it is told it lagged
const CAPACITY: usize = 256;

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
    alive: Rc<Cell<bool>>,
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
    pub(super) async fn join<S>(
        &self,
        open: impl Future<Output = Result<S, ShepherdError>>,
    ) -> Result<broadcast::Receiver<Fan>, ShepherdError>
    where
        S: Stream<Item = Result<BusEvent, Lagged>> + Unpin + 'static,
    {
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref().filter(|session| session.alive.get())
            && let Some(events) = session.events.upgrade()
        {
            return Ok(events.subscribe());
        }
        let mut source = open.await?;
        let (sender, receiver) = broadcast::channel(CAPACITY);
        let events = sender.downgrade();
        let alive = Rc::new(Cell::new(true));
        let flag = Rc::clone(&alive);
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
            flag.set(false);
        });
        *slot = Some(Session { events, alive });
        Ok(receiver)
    }
}

/// The items of `receiver` that `pick` keeps, ending when the subscription does
///
/// A consumer that fell behind is handed [`Fan::Lagged`], as if the shepherd had said so.
pub(super) fn consume<T: 'static>(
    receiver: broadcast::Receiver<Fan>,
    pick: impl Fn(Fan) -> Option<T> + 'static,
) -> LocalBoxStream<'static, T> {
    stream::unfold((receiver, pick), |(mut receiver, pick)| async move {
        loop {
            let fan = match receiver.recv().await {
                Ok(fan) => fan,
                Err(broadcast::error::RecvError::Lagged(_)) => Fan::Lagged,
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            if let Some(item) = pick(fan) {
                return Some((item, (receiver, pick)));
            }
        }
    })
    .boxed_local()
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::rc::Rc;

    use futures_util::stream;
    use shep_client::shep_core::protocol::ProcessInfo;
    use shep_client::shep_core::{protocol::ProcessEventKind, status::ProcStatus};
    use tokio::task::LocalSet;

    use super::*;
    use crate::shepherd::ProcessKind;

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

    fn process_only(fan: Fan) -> Option<ProcessKind> {
        match fan {
            Fan::Process(event) => Some(event.kind),
            _ => None,
        }
    }

    fn config_of_paddock(fan: Fan) -> Option<()> {
        match fan {
            Fan::Config(dog) if dog == "paddock" => Some(()),
            Fan::Lagged => Some(()),
            _ => None,
        }
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
                let mut processes = consume(hub.join(open()).await.expect("joins"), process_only);
                let mut configs =
                    consume(hub.join(open()).await.expect("joins"), config_of_paddock);

                assert_eq!(processes.next().await, Some(ProcessKind::Exit));
                assert_eq!(configs.next().await, Some(()));
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
                let mut processes = consume(hub.join(open()).await.expect("joins"), process_only);
                let mut configs =
                    consume(hub.join(open()).await.expect("joins"), config_of_paddock);

                assert_eq!(processes.next().await, Some(ProcessKind::Exit));
                assert_eq!(processes.next().await, None);
                assert_eq!(configs.next().await, None);

                let _again = hub.join(open()).await.expect("joins");
            })
            .await;
        assert_eq!(opens.get(), 2);
    }
}
