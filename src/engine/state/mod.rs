//! The engine's state between events: the book, who waits for an answer, and what each sheep runs.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use futures_util::{
    future::{AbortHandle, LocalBoxFuture},
    stream::FuturesUnordered,
};
use shep_client::shep_core::values::UpDuration;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use super::{Admission, Clock, Command, InFlight, LeaseEvent};
use crate::{
    book::{Action, Book, Event, LeaseAsk, LeaseId, State, WaiterId},
    config::{Backend, Config, Model, ModelName},
    shepherd::{ProcessEvent, ProcessKind},
};

mod leases;
mod reconcile;
mod saving;

pub(super) use reconcile::Running;

/// Backend work for the run loop to start
#[derive(Debug)]
pub(super) enum Job {
    /// Load the model, within its `load_timeout`.
    Load(Model),
    /// Unload the model, trying again until it is done.
    Unload(Model),
    /// Unload what a timed-out load left, trying again until it is done, then
    /// report the load failed with `error`.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Resolves when a lease stream's reader is dropped, or with `None` once unwatched.
    pub watchers: FuturesUnordered<LocalBoxFuture<'static, Option<Watched>>>,
    /// Each watcher's handle, so a stream that has ended drops the senders watchers hold.
    watching: HashMap<Watched, Vec<AbortHandle>>,
    next_lease: u64,
    /// The model each name was last loaded as, so a model gone from the config still unloads.
    loaded_with: HashMap<ModelName, Model>,
    /// The model last loaded on each sheep.
    on_sheep: HashMap<String, ModelName>,
    /// Sheep the engine stopped whose `Stop` event has not come yet.
    stopping: HashSet<String>,
    /// The model each sheep's skipped quiet stop was for, until a job on that sheep ends.
    stop_skipped: HashMap<String, ModelName>,
    /// How many loads each model has had, so a flock listing taken before a reload is not
    /// read against the reloaded model.
    loads: HashMap<ModelName, u64>,
    jobs: Vec<Job>,
    /// Where `state.json` is written, if anywhere.
    state: Option<PathBuf>,
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
            watching: HashMap::new(),
            next_lease: 1,
            loaded_with: HashMap::new(),
            on_sheep: HashMap::new(),
            stopping: HashSet::new(),
            stop_skipped: HashMap::new(),
            loads: HashMap::new(),
            jobs: Vec::new(),
            state: None,
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
                    self.unwatch(Watched::Waiter(waiter));
                    let _ = events.try_send(LeaseEvent::Granted { lease });
                    self.hold(lease, events);
                }
            }
            Action::Refuse { waiter, refusal } => {
                if let Some(reply) = self.requests.remove(&waiter) {
                    let _ = reply.send(Admission::Refused(refusal));
                } else {
                    self.end_waiting(waiter, LeaseEvent::Refused(refusal));
                }
            }
            Action::Fail { waiter, error } => {
                if let Some(reply) = self.requests.remove(&waiter) {
                    let _ = reply.send(Admission::Failed(error));
                } else {
                    self.end_waiting(waiter, LeaseEvent::Failed(error));
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
                self.unwatch(Watched::Holder(lease));
                if let Some(events) = self.holders.remove(&lease) {
                    let _ = events.try_send(LeaseEvent::Ended(why));
                }
            }
            Action::Persist => self.save(),
        }
    }

    fn load(&mut self, name: ModelName, queue: &mut VecDeque<Event>) {
        let Some(model) = self.config.models.get(&name).cloned() else {
            let error = format!("no model named {name} in the config");
            queue.push_back(Event::LoadFailed { model: name, error });
            return;
        };
        let on_sheep = matches!(model.backend, Backend::Sheep { .. });
        self.seed(model.clone());
        // Saved before the restart, so a dog that dies mid-load knows what the sheep runs.
        if on_sheep {
            self.save();
        }
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

    /// Stops what a load the book no longer waits for left running, without telling the book
    ///
    /// The job replaces any job still running on the model's sheep, or for
    /// the model. A load already queued on that sheep restarts it instead,
    /// and if that load fails, the stop runs then.
    fn stop_quietly(&mut self, model: &ModelName) {
        let Some(loaded) = self.loaded_with.get(model).cloned() else {
            return;
        };
        let restarting = loaded.backend.sheep().filter(|sheep| {
            self.jobs.iter().any(
                |job| matches!(job, Job::Load(queued) if queued.backend.sheep() == Some(sheep)),
            )
        });
        match restarting {
            Some(sheep) => {
                self.stop_skipped.insert(sheep.to_owned(), model.clone());
            }
            None => {
                self.mark_stopping(&loaded);
                self.jobs.push(Job::Unload(loaded));
            }
        }
    }

    /// Feeds back what a job reported
    pub fn finished(&mut self, model: ModelName, outcome: Outcome) {
        let skipped = self
            .loaded_with
            .get(&model)
            .and_then(|loaded| loaded.backend.sheep())
            .and_then(|sheep| self.stop_skipped.remove(sheep));
        let failed = matches!(outcome, Outcome::LoadFailed(_));
        match outcome {
            Outcome::Loaded if self.book.state(&model) == Some(State::Loading) => {
                self.feed(Event::Loaded { model });
            }
            Outcome::Loaded => self.stop_quietly(&model),
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
            // A quiet stop's result: the book never asked for it.
            Outcome::Unloaded if self.book.state(&model) != Some(State::Unloading) => {}
            Outcome::Unloaded => self.feed(Event::Unloaded { model }),
        }
        // A failed load may not have restarted the sheep, so the process before it may run on.
        if let Some(skipped) = skipped.filter(|_| failed) {
            self.stop_quietly(&skipped);
        }
    }

    /// Reads a sheep's lifecycle event, and tells the book of a backend that went down
    ///
    /// A start clears the engine's own stop mark: shep publishes the `Stop`
    /// of a stop it carried out before any later start of that sheep.
    pub fn process(&mut self, event: ProcessEvent) {
        match event.kind {
            ProcessKind::Started | ProcessKind::Online => {
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
        let state = self.book.state(&model);
        if !matches!(
            state,
            Some(State::Loaded | State::Loading | State::Evicting)
        ) {
            return;
        }
        eprintln!(
            "paddock: sheep {} serving {model} went down ({:?}, manually: {})",
            event.sheep, event.kind, event.manually
        );
        self.feed(Event::BackendExited {
            model: model.clone(),
        });
        // A load the book gave up on may still reach ready and hold memory counted free.
        if state == Some(State::Loading) && self.book.state(&model) == Some(State::Unloaded) {
            self.stop_quietly(&model);
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
