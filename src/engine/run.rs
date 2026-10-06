//! The engine's loop, the backend work it runs, and its subscription to process events.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures_util::{
    FutureExt as _, StreamExt as _,
    future::{AbortHandle, LocalBoxFuture, abortable},
    stream::{FuturesUnordered, LocalBoxStream},
};
use shep_client::dogs::Stop;
use tokio::time::{Instant, sleep, sleep_until, timeout};

use super::{
    Inbox, Start,
    state::{Engine, Job, Outcome, Running},
};
use crate::{
    backend::Backends,
    book::Event,
    config::{Backend, Config, Model, ModelName},
    shepherd::{ProcessEvent, Shepherd, ShepherdError},
};
use shep_client::shep_core::protocol::ProcessInfo;

/// How long the engine waits to ask the shepherd again for process events after it refused,
/// and the least time between two subscriptions.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
// A failed unload leaves the model counted as holding its memory until one
// succeeds, so it is tried again at this pace for as long as it fails.
const UNLOAD_RETRY: Duration = Duration::from_secs(5);
// A backend that accepts an unload and never answers would hold the model's memory in the
// book forever. Ollama unloads in seconds even for a large model, so thirty is a hung one.
const UNLOAD_ATTEMPT: Duration = Duration::from_secs(30);

/// Runs the engine until `stop` is requested or every [`EngineHandle`](super::EngineHandle) is gone
///
/// One task owns the book. Backend work runs on futures this task polls,
/// since the shepherd's futures are not `Send`, so `run` itself is spawned
/// with `spawn_local` or awaited in place, never with `tokio::spawn`.
/// The book starts from `start`'s saved leases and discovered models.
pub(crate) async fn run<S: Shepherd>(
    config: Arc<Config>,
    backends: Backends<S>,
    start: Start,
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
    engine.restore(start);
    let mut jobs = Jobs::new(&backends);
    let mut events = Events::new(backends.shepherd());
    let mut listing: Option<Listing<'_>> = None;
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
            heard = events.next() => match heard {
                Heard::Event(event) => engine.process(event),
                Heard::Subscribed => {
                    engine.drop_stale_marks(&jobs.stopping());
                    listing = Some(Listing {
                        expected: engine.expected_running(),
                        flock: backends.shepherd().list_flock().boxed_local(),
                    });
                }
            },
            (expected, flock) = listed(&mut listing) => match flock {
                Ok(flock) => engine.reconcile(expected, &flock),
                Err(err) => eprintln!("paddock: listing the flock failed: {err}"),
            },
            Some(watched) = engine.watchers.next() => {
                if let Some(watched) = watched {
                    engine.hung_up(watched);
                }
            }
            Some(notice) = notices.recv() => engine.command(notice),
            command = commands.recv() => match command {
                Some(command) => engine.command(command),
                None => return,
            },
            () = at(deadline) => engine.feed(Event::Tick),
        }
    }
}

/// A flock listing under way, and the sheep expected running when it was asked for
struct Listing<'a> {
    expected: Vec<Running>,
    flock: LocalBoxFuture<'a, Result<Vec<ProcessInfo>, ShepherdError>>,
}

/// The listing's result once it comes, or never without one
///
/// # Cancellation safety
/// Safe: the listing stays in `listing` until it has finished.
async fn listed(
    listing: &mut Option<Listing<'_>>,
) -> (Vec<Running>, Result<Vec<ProcessInfo>, ShepherdError>) {
    let Some(under_way) = listing else {
        return core::future::pending().await;
    };
    let flock = (&mut under_way.flock).await;
    let expected = core::mem::take(&mut under_way.expected);
    *listing = None;
    (expected, flock)
}

async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => core::future::pending().await,
    }
}

type Work<'a> = LocalBoxFuture<'a, Option<(JobKey, u64, ModelName, Outcome)>>;

/// What a job is kept under in [`Jobs`]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum JobKey {
    /// A sheep's job, whichever of its models it is for, so its starts and stops run in order.
    Sheep(String),
    /// An ollama model's job.
    Model(ModelName),
}

/// Backend work under way, at most one job per sheep or ollama model
///
/// A new job under a key drops the one before it, and a result from a
/// dropped job is never reported.
struct Jobs<'a, S> {
    backends: &'a Backends<S>,
    running: FuturesUnordered<Work<'a>>,
    /// Each key's job, and the sheep it stops when it stops one.
    current: HashMap<JobKey, (u64, AbortHandle, Option<String>)>,
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
        let stops = match &job {
            Job::Load(_) => None,
            Job::Unload(model) | Job::Cleanup(model, _) => match &model.backend {
                Backend::Sheep { sheep, .. } => Some(sheep.clone()),
                Backend::Ollama { .. } => None,
            },
        };
        let (Job::Load(model) | Job::Unload(model) | Job::Cleanup(model, _)) = &job;
        let key = key(model);
        let (model, work) = match job {
            Job::Load(model) => (model.name.clone(), load(self.backends, model)),
            Job::Unload(model) => (model.name.clone(), unload(self.backends, model)),
            Job::Cleanup(model, error) => {
                (model.name.clone(), cleanup(self.backends, model, error))
            }
        };
        let (work, handle) = abortable(work);
        if let Some((_, before, _)) = self.current.insert(key.clone(), (id, handle, stops)) {
            before.abort();
        }
        self.running.push(
            async move { work.await.ok().map(|outcome| (key, id, model, outcome)) }.boxed_local(),
        );
    }

    /// The sheep a running job is stopping
    fn stopping(&self) -> HashSet<String> {
        self.current
            .values()
            .filter_map(|(_, _, stops)| stops.clone())
            .collect()
    }

    /// The next result of a job not replaced since it started, or `None` with nothing running
    ///
    /// # Cancellation safety
    /// Safe: a result is taken from the set only when it is returned or dropped as stale.
    async fn next(&mut self) -> Option<(ModelName, Outcome)> {
        loop {
            let Some((key, id, model, outcome)) = self.running.next().await? else {
                continue;
            };
            if self
                .current
                .get(&key)
                .is_some_and(|(current, _, _)| *current == id)
            {
                self.current.remove(&key);
                return Some((model, outcome));
            }
        }
    }
}

/// The key `model`'s jobs are kept under
fn key(model: &Model) -> JobKey {
    match &model.backend {
        Backend::Sheep { sheep, .. } => JobKey::Sheep(sheep.clone()),
        Backend::Ollama { .. } => JobKey::Model(model.name.clone()),
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

/// One unload, given up on after [`UNLOAD_ATTEMPT`]
async fn unload_attempt<S: Shepherd>(backends: &Backends<S>, model: &Model) -> Result<(), String> {
    match timeout(UNLOAD_ATTEMPT, backends.unload(model)).await {
        Ok(result) => result.map_err(|err| err.to_string()),
        Err(_) => Err(format!("no answer in {}s", UNLOAD_ATTEMPT.as_secs())),
    }
}

/// Unloads `model`, trying again every [`UNLOAD_RETRY`] until it is done
async fn unload_until_done<S: Shepherd>(backends: &Backends<S>, model: &Model) {
    while let Err(err) = unload_attempt(backends, model).await {
        eprintln!(
            "paddock: unloading {} failed, trying again: {err}",
            model.name
        );
        sleep(UNLOAD_RETRY).await;
    }
}

fn unload<S: Shepherd>(backends: &Backends<S>, model: Model) -> LocalBoxFuture<'_, Outcome> {
    async move {
        unload_until_done(backends, &model).await;
        Outcome::Unloaded
    }
    .boxed_local()
}

/// Stops what a timed-out load left, then reports the load failed
///
/// Until a stop succeeds the book counts the model as loading. Its memory
/// stays counted.
fn cleanup<S: Shepherd>(
    backends: &Backends<S>,
    model: Model,
    error: String,
) -> LocalBoxFuture<'_, Outcome> {
    async move {
        unload_until_done(backends, &model).await;
        Outcome::LoadFailed(error)
    }
    .boxed_local()
}

type Subscribing<'a> =
    LocalBoxFuture<'a, Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError>>;

/// What the subscription yields
enum Heard {
    /// A process event.
    Event(ProcessEvent),
    /// A subscription opened, so events from before it may have been missed.
    Subscribed,
}

/// The subscription to process events, taken out again whenever it ends
enum Feed<'a> {
    Subscribing(Subscribing<'a>),
    /// A subscription, and when it opened.
    Open(LocalBoxStream<'static, ProcessEvent>, Instant),
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

    /// The next process event, or word of a new subscription, subscribing again as often as
    /// the subscription ends
    ///
    /// # Cancellation safety
    /// Safe: a subscription under way is kept in `feed`, and a stream loses nothing when its
    /// `next` is dropped.
    async fn next(&mut self) -> Heard {
        loop {
            match &mut self.feed {
                Feed::Subscribing(subscribing) => match subscribing.await {
                    Ok(stream) => {
                        self.feed = Feed::Open(stream, Instant::now());
                        return Heard::Subscribed;
                    }
                    Err(err) => {
                        eprintln!("paddock: subscribing to process events failed: {err}");
                        self.feed = Feed::Retrying(Instant::now() + RESUBSCRIBE_DELAY);
                    }
                },
                Feed::Open(stream, opened) => match stream.next().await {
                    Some(event) => return Heard::Event(event),
                    None => {
                        eprintln!("paddock: process events ended; subscribing again");
                        // One that ended at once is waited out, so a connection that keeps
                        // dying is not asked again in a tight loop.
                        let again = *opened + RESUBSCRIBE_DELAY;
                        self.feed = if Instant::now() < again {
                            Feed::Retrying(again)
                        } else {
                            Feed::Subscribing(self.shepherd.process_events().boxed_local())
                        };
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

#[cfg(test)]
mod tests;
