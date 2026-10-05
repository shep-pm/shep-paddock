//! The engine's loop, the backend work it runs, and its subscription to process events.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use futures_util::{
    FutureExt as _, StreamExt as _,
    future::{AbortHandle, LocalBoxFuture, abortable},
    stream::{FuturesUnordered, LocalBoxStream},
};
use shep_client::dogs::Stop;
use tokio::time::{Instant, sleep, sleep_until, timeout};

use super::{
    Inbox,
    state::{Engine, Job, Outcome},
};
use crate::{
    backend::Backends,
    book::Event,
    config::{Config, Model, ModelName},
    shepherd::{ProcessEvent, Shepherd, ShepherdError},
};

/// How long the engine waits to ask the shepherd again for process events after it refused.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
// A failed unload leaves the model counted as holding its memory until one
// succeeds, so it is tried again at this pace for as long as it fails.
const UNLOAD_RETRY: Duration = Duration::from_secs(5);

/// Runs the engine until `stop` is requested or every [`EngineHandle`](super::EngineHandle) is gone
///
/// One task owns the book. Backend work runs on futures this task polls,
/// since the shepherd's futures are not `Send`, so `run` itself is spawned
/// with `spawn_local` or awaited in place, never with `tokio::spawn`.
/// `_state` names where `state.json` lives; leases are not saved to it.
pub(crate) async fn run<S: Shepherd>(
    config: Arc<Config>,
    backends: Backends<S>,
    _state: Option<PathBuf>,
    inbox: Inbox,
    mut stop: Stop,
) {
    let Inbox {
        mut commands,
        mut notices,
        notify,
        clock,
    } = inbox;
    let mut engine = Engine::new(config, clock, notify);
    let mut jobs = Jobs::new(&backends);
    let mut events = Events::new(backends.shepherd());
    loop {
        for job in engine.take_jobs() {
            jobs.start(job);
        }
        let deadline = engine.next_deadline();
        // Biased so what the backends and the shepherd report is applied before
        // the next command is answered. The tick comes last, so a deadline that
        // fires again at once cannot keep a command from being answered.
        tokio::select! {
            biased;
            () = stop.wait() => return,
            Some((model, outcome)) = jobs.next() => engine.finished(model, outcome),
            event = events.next() => engine.process(event),
            Some(watched) = engine.watchers.next() => engine.hung_up(watched),
            Some(notice) = notices.recv() => engine.command(notice),
            command = commands.recv() => match command {
                Some(command) => engine.command(command),
                None => return,
            },
            () = at(deadline) => engine.feed(Event::Tick),
        }
    }
}

async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => core::future::pending().await,
    }
}

type Running<'a> = LocalBoxFuture<'a, Option<(ModelName, u64, Outcome)>>;

/// Backend work under way, at most one job per model
///
/// A new job for a model drops the one before it, and a result from a
/// dropped job is never reported.
struct Jobs<'a, S> {
    backends: &'a Backends<S>,
    running: FuturesUnordered<Running<'a>>,
    current: HashMap<ModelName, (u64, AbortHandle)>,
    started: u64,
}

impl<'a, S: Shepherd> Jobs<'a, S> {
    fn new(backends: &'a Backends<S>) -> Self {
        Jobs {
            backends,
            running: FuturesUnordered::new(),
            current: HashMap::new(),
            started: 0,
        }
    }

    fn start(&mut self, job: Job) {
        self.started += 1;
        let id = self.started;
        let (model, work) = match job {
            Job::Load(model) => (model.name.clone(), load(self.backends, model)),
            Job::Unload(model) => (model.name.clone(), unload(self.backends, model)),
            Job::Cleanup(model, error) => {
                (model.name.clone(), cleanup(self.backends, model, error))
            }
        };
        let (work, handle) = abortable(work);
        if let Some((_, before)) = self.current.insert(model.clone(), (id, handle)) {
            before.abort();
        }
        self.running
            .push(async move { work.await.ok().map(|outcome| (model, id, outcome)) }.boxed_local());
    }

    /// The next result of a job not replaced since it started, or `None` with nothing running
    ///
    /// # Cancellation safety
    /// Safe: a result is taken from the set only when it is returned or dropped as stale.
    async fn next(&mut self) -> Option<(ModelName, Outcome)> {
        loop {
            let Some((model, id, outcome)) = self.running.next().await? else {
                continue;
            };
            if self
                .current
                .get(&model)
                .is_some_and(|(current, _)| *current == id)
            {
                self.current.remove(&model);
                return Some((model, outcome));
            }
        }
    }
}

fn load<S: Shepherd>(backends: &Backends<S>, model: Model) -> LocalBoxFuture<'_, Outcome> {
    async move {
        match timeout(model.load_timeout, backends.load(&model)).await {
            Ok(Ok(())) => Outcome::Loaded,
            Ok(Err(err)) => Outcome::LoadFailed(err.to_string()),
            Err(_) => Outcome::TimedOut(model.load_timeout),
        }
    }
    .boxed_local()
}

fn unload<S: Shepherd>(backends: &Backends<S>, model: Model) -> LocalBoxFuture<'_, Outcome> {
    async move {
        while let Err(err) = backends.unload(&model).await {
            eprintln!(
                "paddock: unloading {} failed, trying again: {err}",
                model.name
            );
            sleep(UNLOAD_RETRY).await;
        }
        Outcome::Unloaded
    }
    .boxed_local()
}

fn cleanup<S: Shepherd>(
    backends: &Backends<S>,
    model: Model,
    error: String,
) -> LocalBoxFuture<'_, Outcome> {
    async move {
        if let Err(err) = backends.unload(&model).await {
            eprintln!(
                "paddock: stopping {} after its load timed out failed: {err}",
                model.name
            );
        }
        Outcome::LoadFailed(error)
    }
    .boxed_local()
}

type Subscribing<'a> =
    LocalBoxFuture<'a, Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError>>;

/// The subscription to process events, taken out again whenever it ends
enum Feed<'a> {
    Subscribing(Subscribing<'a>),
    Open(LocalBoxStream<'static, ProcessEvent>),
    Retrying(Instant),
}

struct Events<'a, S> {
    shepherd: &'a S,
    feed: Feed<'a>,
}

impl<'a, S: Shepherd> Events<'a, S> {
    fn new(shepherd: &'a S) -> Self {
        Events {
            shepherd,
            feed: Feed::Subscribing(shepherd.process_events().boxed_local()),
        }
    }

    /// The next process event, subscribing again as often as the subscription ends
    ///
    /// # Cancellation safety
    /// Safe: a subscription under way is kept in `feed`, and a stream loses nothing when its
    /// `next` is dropped.
    async fn next(&mut self) -> ProcessEvent {
        loop {
            match &mut self.feed {
                Feed::Subscribing(subscribing) => match subscribing.await {
                    Ok(stream) => self.feed = Feed::Open(stream),
                    Err(err) => {
                        eprintln!("paddock: subscribing to process events failed: {err}");
                        self.feed = Feed::Retrying(Instant::now() + RESUBSCRIBE_DELAY);
                    }
                },
                Feed::Open(stream) => match stream.next().await {
                    Some(event) => return event,
                    None => {
                        eprintln!("paddock: process events ended; subscribing again");
                        self.feed = Feed::Subscribing(self.shepherd.process_events().boxed_local());
                    }
                },
                Feed::Retrying(at) => {
                    sleep_until(*at).await;
                    self.feed = Feed::Subscribing(self.shepherd.process_events().boxed_local());
                }
            }
        }
    }
}
