//! The engine's state between events: the book, who waits for an answer, and what each sheep runs.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use futures_util::{FutureExt as _, future::LocalBoxFuture, stream::FuturesUnordered};
use shep_client::shep_core::values::UpDuration;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use super::{Admission, Clock, Command, InFlight, LeaseEvent, LeaseRefused};
use crate::{
    book::{Action, Book, Event, Hold, LeaseAsk, LeaseId, State, WaiterId},
    config::{Backend, ClientName, Config, Model, ModelName},
    shepherd::{ProcessEvent, ProcessKind},
};

mod reconcile;

pub(super) use reconcile::Running;

/// Backend work for the run loop to start
#[derive(Debug)]
pub(super) enum Job {
    /// Load the model, within its `load_timeout`.
    Load(Model),
    /// Unload the model, trying again until it is done.
    Unload(Model),
    /// Unload what a timed-out load left, then report the load failed with `error`.
    Cleanup(Model, String),
}

/// What a job reports
#[derive(Debug)]
pub(super) enum Outcome {
    Loaded,
    LoadFailed(String),
    /// The load was not ready within this long.
    TimedOut(Duration),
    Unloaded,
}

/// A lease stream whose reader may have gone
#[derive(Debug, Clone, Copy)]
pub(super) enum Watched {
    /// A lease still waiting to be granted.
    Waiter(WaiterId),
    /// A granted lease's holder.
    Holder(LeaseId),
}

/// The book, and everything the engine keeps to act on its decisions
pub(super) struct Engine {
    pub clock: Clock,
    config: Arc<Config>,
    pub book: Book,
    /// Where each `InFlight` reports its request finished.
    notify: mpsc::UnboundedSender<Command>,
    requests: HashMap<WaiterId, oneshot::Sender<Admission>>,
    waiting_leases: HashMap<WaiterId, mpsc::Sender<LeaseEvent>>,
    holders: HashMap<LeaseId, mpsc::Sender<LeaseEvent>>,
    /// Resolves when a lease stream's reader is dropped.
    pub watchers: FuturesUnordered<LocalBoxFuture<'static, Watched>>,
    next_lease: u64,
    /// The model each name was last loaded as, so a model gone from the config still unloads.
    loaded_with: HashMap<ModelName, Model>,
    /// The model last loaded on each sheep.
    on_sheep: HashMap<String, ModelName>,
    /// Sheep the engine stopped whose `Stop` event has not come yet.
    stopping: HashSet<String>,
    /// How many loads each model has had, so a flock listing taken before a reload is not
    /// read against the reloaded model.
    loads: HashMap<ModelName, u64>,
    jobs: Vec<Job>,
}

impl Engine {
    pub fn new(config: Arc<Config>, clock: Clock, notify: mpsc::UnboundedSender<Command>) -> Self {
        Engine {
            clock,
            book: Book::new(Arc::clone(&config)),
            config,
            notify,
            requests: HashMap::new(),
            waiting_leases: HashMap::new(),
            holders: HashMap::new(),
            watchers: FuturesUnordered::new(),
            next_lease: 1,
            loaded_with: HashMap::new(),
            on_sheep: HashMap::new(),
            stopping: HashSet::new(),
            loads: HashMap::new(),
            jobs: Vec::new(),
        }
    }

    /// The backend work the last events called for
    pub fn take_jobs(&mut self) -> Vec<Job> {
        core::mem::take(&mut self.jobs)
    }

    /// When the book next wants a `Tick`
    pub fn next_deadline(&self) -> Option<Instant> {
        self.book
            .next_deadline()
            .and_then(|moment| self.clock.instant(moment))
    }

    /// A lease id past every lease the book holds, restored ones included
    pub fn next_lease(&mut self) -> LeaseId {
        let past_book = self
            .book
            .max_lease_id()
            .map_or(1, |id| id.0.saturating_add(1));
        let id = self.next_lease.max(past_book);
        self.next_lease = id.saturating_add(1);
        LeaseId(id)
    }

    /// Applies `event` and every event its actions report at once
    pub fn feed(&mut self, event: Event) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let actions = self.book.handle(self.clock.moment(), event);
            self.apply(actions, &mut queue);
        }
    }

    pub fn apply(&mut self, actions: Vec<Action>, queue: &mut VecDeque<Event>) {
        for action in actions {
            self.act(action, queue);
        }
    }

    fn act(&mut self, action: Action, queue: &mut VecDeque<Event>) {
        match action {
            Action::Load(model) => self.load(model, queue),
            Action::Unload(model) => self.unload(model, queue),
            Action::Forward { waiter, model } => {
                // Made even with nobody to take it, so its drop balances the book's count.
                let in_flight = InFlight::new(model, self.notify.clone());
                if let Some(reply) = self.requests.remove(&waiter) {
                    let _ = reply.send(Admission::Forward(in_flight));
                }
            }
            Action::Grant { waiter, lease } => {
                if let Some(events) = self.waiting_leases.remove(&waiter) {
                    let _ = events.try_send(LeaseEvent::Granted { lease });
                    self.hold(lease, events);
                }
            }
            Action::Refuse { waiter, refusal } => {
                if let Some(reply) = self.requests.remove(&waiter) {
                    let _ = reply.send(Admission::Refused(refusal));
                } else if let Some(events) = self.waiting_leases.remove(&waiter) {
                    let _ = events.try_send(LeaseEvent::Refused(refusal));
                }
            }
            Action::Fail { waiter, error } => {
                if let Some(reply) = self.requests.remove(&waiter) {
                    let _ = reply.send(Admission::Failed(error));
                } else if let Some(events) = self.waiting_leases.remove(&waiter) {
                    let _ = events.try_send(LeaseEvent::Failed(error));
                }
            }
            Action::Waiting {
                waiter,
                reason,
                estimate,
            } => {
                if let Some(events) = self.waiting_leases.get(&waiter) {
                    let estimate = estimate.map(|moment| self.clock.wall(moment));
                    let _ = events.try_send(LeaseEvent::Waiting { reason, estimate });
                }
            }
            Action::LeaseEnded { lease, why } => {
                if let Some(events) = self.holders.remove(&lease) {
                    let _ = events.try_send(LeaseEvent::Ended(why));
                }
            }
            // The engine writes no state.json, so leases do not outlive it.
            Action::Persist => {}
        }
    }

    fn load(&mut self, name: ModelName, queue: &mut VecDeque<Event>) {
        let Some(model) = self.config.models.get(&name).cloned() else {
            let error = format!("no model named {name} in the config");
            queue.push_back(Event::LoadFailed { model: name, error });
            return;
        };
        if let Backend::Sheep { sheep, .. } = &model.backend {
            self.on_sheep.insert(sheep.clone(), name.clone());
        }
        *self.loads.entry(name.clone()).or_default() += 1;
        self.loaded_with.insert(name, model.clone());
        self.jobs.push(Job::Load(model));
    }

    fn unload(&mut self, name: ModelName, queue: &mut VecDeque<Event>) {
        let known = self
            .loaded_with
            .get(&name)
            .or(self.config.models.get(&name));
        let Some(model) = known.cloned() else {
            eprintln!("paddock: no backend is known for {name}, so it counts as unloaded");
            queue.push_back(Event::Unloaded { model: name });
            return;
        };
        self.mark_stopping(&model);
        self.jobs.push(Job::Unload(model));
    }

    fn mark_stopping(&mut self, model: &Model) {
        if let Backend::Sheep { sheep, .. } = &model.backend {
            self.stopping.insert(sheep.clone());
        }
    }

    fn hold(&mut self, lease: LeaseId, events: mpsc::Sender<LeaseEvent>) {
        self.watch(Watched::Holder(lease), events.clone());
        self.holders.insert(lease, events);
    }

    fn watch(&mut self, watched: Watched, events: mpsc::Sender<LeaseEvent>) {
        self.watchers.push(
            async move {
                events.closed().await;
                watched
            }
            .boxed_local(),
        );
    }

    /// Feeds back what a job reported
    pub fn finished(&mut self, model: ModelName, outcome: Outcome) {
        match outcome {
            Outcome::Loaded => self.feed(Event::Loaded { model }),
            Outcome::LoadFailed(error) => self.feed(Event::LoadFailed { model, error }),
            Outcome::TimedOut(after) => {
                let after = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
                let error = format!("not ready after {}", UpDuration::from_millis(after));
                match self.loaded_with.get(&model).cloned() {
                    Some(loading) => {
                        self.mark_stopping(&loading);
                        self.jobs.push(Job::Cleanup(loading, error));
                    }
                    None => self.feed(Event::LoadFailed { model, error }),
                }
            }
            Outcome::Unloaded => self.feed(Event::Unloaded { model }),
        }
    }

    /// Reads a sheep's lifecycle event, and tells the book of a backend that went down
    ///
    /// A start clears the engine's own stop mark, since shep publishes any
    /// `Stop` of the previous process before it.
    pub fn process(&mut self, event: ProcessEvent) {
        match event.kind {
            ProcessKind::Started => {
                self.stopping.remove(&event.sheep);
                return;
            }
            ProcessKind::Stop if self.stopping.remove(&event.sheep) => return,
            ProcessKind::Exit | ProcessKind::Errored | ProcessKind::Stop => {}
            ProcessKind::Other => return,
        }
        if self.stopping.contains(&event.sheep) {
            return;
        }
        let Some(model) = self.on_sheep.get(&event.sheep).cloned() else {
            return;
        };
        if matches!(
            self.book.state(&model),
            Some(State::Loaded | State::Loading | State::Evicting)
        ) {
            eprintln!(
                "paddock: sheep {} serving {model} went down ({:?}, manually: {})",
                event.sheep, event.kind, event.manually
            );
            self.feed(Event::BackendExited { model });
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

    pub fn command(&mut self, command: Command) {
        match command {
            Command::Admit {
                waiter,
                client,
                model,
                priority,
                max_wait,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                if !self.config.models.contains_key(&model) {
                    let _ = reply.send(Admission::Unknown);
                    return;
                }
                self.requests.insert(waiter, reply);
                self.feed(Event::RequestArrived {
                    waiter,
                    client,
                    model,
                    priority,
                    max_wait,
                });
            }
            Command::TakeLease {
                waiter,
                client,
                ask,
                events,
            } => {
                let ask = LeaseAsk {
                    lease: self.next_lease(),
                    client,
                    model: ask.model,
                    priority: ask.priority,
                    expected: ask.expected,
                    max_wait: ask.max_wait,
                    hold: ask.hold,
                    note: ask.note,
                };
                self.watch(Watched::Waiter(waiter), events.clone());
                self.waiting_leases.insert(waiter, events);
                self.feed(Event::LeaseAsked { waiter, ask });
            }
            Command::Attach {
                client,
                lease,
                events,
                reply,
            } => {
                let attached = self.attach(&client, lease, events);
                let _ = reply.send(attached);
            }
            Command::Renew {
                client,
                lease,
                reply,
            } => {
                let renewed = self
                    .owned(&client, lease)
                    .map(|()| self.feed(Event::LeaseRenewed { lease }));
                let _ = reply.send(renewed);
            }
            Command::Release {
                client,
                lease,
                reply,
            } => {
                let released = self
                    .owned(&client, lease)
                    .map(|()| self.feed(Event::LeaseReleased { lease }));
                let _ = reply.send(released);
            }
            Command::Snapshot { reply } => {
                let _ = reply.send(self.book.snapshot(self.clock.moment()));
            }
            Command::Reconfigure { config, done } => {
                self.config = Arc::clone(&config);
                let actions = self.book.reconfigure(self.clock.moment(), config);
                let mut queue = VecDeque::new();
                self.apply(actions, &mut queue);
                while let Some(event) = queue.pop_front() {
                    self.feed(event);
                }
                let _ = done.send(());
            }
            Command::WaiterGone { waiter } => {
                self.requests.remove(&waiter);
                self.feed(Event::WaiterGone { waiter });
            }
            Command::Finished { model } => self.feed(Event::RequestFinished { model }),
        }
    }

    /// Whether `client` holds the granted lease, once leases past their end have ended
    fn owned(&mut self, client: &ClientName, lease: LeaseId) -> Result<(), LeaseRefused> {
        self.feed(Event::Tick);
        match self.book.lease(lease) {
            None => Err(LeaseRefused::NotFound),
            Some(view) if view.client != *client => Err(LeaseRefused::NotYours),
            Some(_) => Ok(()),
        }
    }

    fn attach(
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

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine")
            .field("book", &self.book)
            .field("holders", &self.holders.keys().collect::<Vec<_>>())
            .field("stopping", &self.stopping)
            .finish_non_exhaustive()
    }
}
