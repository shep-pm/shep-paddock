//! A fake shepherd that records what the dog asks of it.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use futures_util::{
    StreamExt as _,
    stream::{self, LocalBoxStream},
};
use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};
use tokio::sync::{
    Semaphore,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use crate::shepherd::{ProcessEvent, ProcessKind, Shepherd, ShepherdError};

/// One call the backends made of the shepherd, in the order made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    SetEnv(String, String, String),
    SetField(String, String, serde_json::Value),
    Restart(String),
    Stop(String),
}

/// A shepherd that records what it is asked, so a test can assert the exact order of calls
/// without a daemon, and refuses restarts on request. Its flock lists each sheep as the calls,
/// [`Self::running`] and [`Self::crash`] left it. Its dog section is empty until [`Self::set_section`].
///
/// Each `process_events` call takes the next subscription [`Self::feed`] or
/// [`Self::refuse_subscription`] queued. A fed one yields what the test sends and ends when the
/// test drops the sender. With none queued, the subscription stays open and quiet.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeShepherd {
    calls: Arc<Mutex<Vec<Call>>>,
    refuse_restart: Option<String>,
    stall_restart: bool,
    /// Each restart waits for a permit, which [`Self::open_gate`] adds.
    gate: Option<Arc<Semaphore>>,
    failing_stops: Arc<Mutex<usize>>,
    /// `None` is a subscription the shepherd refuses.
    feeds: Arc<Mutex<VecDeque<Option<UnboundedReceiver<ProcessEvent>>>>>,
    subscribed: Arc<Mutex<usize>>,
    /// Each sheep's status as the calls left it, which `list_flock` reports.
    flock: Arc<Mutex<BTreeMap<String, ProcStatus>>>,
    listings: Arc<Mutex<usize>>,
    /// Leave each restarted sheep Starting, for [`Self::come_online`] to finish.
    start_without_online: bool,
    /// Each `sheep_online` subscription still open.
    online: Arc<Mutex<Vec<UnboundedSender<ProcessEvent>>>>,
    /// Each running sheep's pid, which `list_flock` and `online` events report.
    pids: Arc<Mutex<BTreeMap<String, u32>>>,
    last_pid: Arc<Mutex<u32>>,
    /// What `dog_config` answers.
    section: Arc<Mutex<String>>,
    section_reads: Arc<Mutex<usize>>,
    /// As `feeds`, for `config_changes`.
    config_feeds: Arc<Mutex<VecDeque<Option<UnboundedReceiver<()>>>>>,
    config_subscribed: Arc<Mutex<usize>>,
}

impl FakeShepherd {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Makes every restart answer `Refused` with `what`.
    pub(crate) fn refusing_restart(what: &str) -> Self {
        Self {
            refuse_restart: Some(what.to_owned()),
            ..Self::default()
        }
    }

    /// Makes every restart wait forever after it is recorded, as a backend that never
    /// becomes ready does.
    pub(crate) fn stalling_restart() -> Self {
        Self {
            stall_restart: true,
            ..Self::default()
        }
    }

    /// Makes every restart answer with the sheep still Starting, as one whose
    /// probe has not passed: it comes online only through [`Self::come_online`].
    pub(crate) fn starting_restart() -> Self {
        Self {
            start_without_online: true,
            ..Self::default()
        }
    }

    /// Marks `sheep` online and tells every `sheep_online` subscription, with its current pid.
    pub(crate) fn come_online(&self, sheep: &str) {
        self.set_status(sheep, ProcStatus::Online);
        let event = ProcessEvent {
            sheep: sheep.to_owned(),
            kind: ProcessKind::Online,
            manually: false,
            pid: self.pid_of(sheep),
        };
        self.online
            .lock()
            .expect("online lock")
            .retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn pid_of(&self, sheep: &str) -> Option<u32> {
        self.pids.lock().expect("pids lock").get(sheep).copied()
    }

    /// Gives `sheep` a new process, as a start does, and returns its pid.
    fn spawn(&self, sheep: &str) -> u32 {
        let mut last = self.last_pid.lock().expect("last pid lock");
        *last += 1;
        self.pids
            .lock()
            .expect("pids lock")
            .insert(sheep.to_owned(), *last);
        *last
    }

    /// Marks `sheep` online and ends every `sheep_online` subscription unheard,
    /// as a dropped connection or a lag would.
    pub(crate) fn come_online_unheard(&self, sheep: &str) {
        self.set_status(sheep, ProcStatus::Online);
        self.online.lock().expect("online lock").clear();
    }

    /// Marks `sheep` stopped and ends every `sheep_online` subscription unheard,
    /// as a sheep that exits for good while the connection is down.
    pub(crate) fn stop_unheard(&self, sheep: &str) {
        self.pids.lock().expect("pids lock").remove(sheep);
        self.set_status(sheep, ProcStatus::Stopped);
        self.online.lock().expect("online lock").clear();
    }

    /// Makes every restart wait, after it is recorded, until [`Self::open_gate`] lets one through.
    pub(crate) fn gated_restart() -> Self {
        Self::default().gated()
    }

    /// As [`Self::gated_restart`], on a fake already set up otherwise.
    pub(crate) fn gated(self) -> Self {
        Self {
            gate: Some(Arc::new(Semaphore::new(0))),
            ..self
        }
    }

    /// Lets one waiting or later restart through.
    pub(crate) fn open_gate(&self) {
        if let Some(gate) = &self.gate {
            gate.add_permits(1);
        }
    }

    /// Makes the next `times` stops fail after they are recorded.
    pub(crate) fn failing_stops(times: usize) -> Self {
        Self {
            failing_stops: Arc::new(Mutex::new(times)),
            ..Self::default()
        }
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// Queues the subscription the next `process_events` call gets, and returns its sender.
    pub(crate) fn feed(&self) -> UnboundedSender<ProcessEvent> {
        let (tx, rx) = unbounded_channel();
        self.feeds.lock().expect("feeds lock").push_back(Some(rx));
        tx
    }

    /// Makes the next `process_events` call fail.
    pub(crate) fn refuse_subscription(&self) {
        self.feeds.lock().expect("feeds lock").push_back(None);
    }

    /// Marks `sheep` online without an event, as one started before the dog was.
    pub(crate) fn running(&self, sheep: &str) {
        self.spawn(sheep);
        self.set_status(sheep, ProcStatus::Online);
    }

    /// Marks `sheep` waiting for shep to start it again after a crash, without an event.
    pub(crate) fn waiting_restart(&self, sheep: &str) {
        self.set_status(sheep, ProcStatus::WaitingRestart);
    }

    /// Marks `sheep` errored without an event, as a crash the subscription missed.
    pub(crate) fn crash(&self, sheep: &str) {
        self.set_status(sheep, ProcStatus::Errored);
    }

    /// How many times `list_flock` was called.
    pub(crate) fn listings(&self) -> usize {
        *self.listings.lock().expect("listings lock")
    }

    /// Sets the text `dog_config` answers with.
    pub(crate) fn set_section(&self, text: &str) {
        text.clone_into(&mut self.section.lock().expect("section lock"));
    }

    /// How many times `dog_config` was called.
    pub(crate) fn section_reads(&self) -> usize {
        *self.section_reads.lock().expect("section reads lock")
    }

    /// Queues the subscription the next `config_changes` call gets, and returns its sender.
    pub(crate) fn config_feed(&self) -> UnboundedSender<()> {
        let (tx, rx) = unbounded_channel();
        self.config_feeds
            .lock()
            .expect("config feeds lock")
            .push_back(Some(rx));
        tx
    }

    /// Makes the next `config_changes` call fail.
    pub(crate) fn refuse_config_subscription(&self) {
        self.config_feeds
            .lock()
            .expect("config feeds lock")
            .push_back(None);
    }

    /// How many times `config_changes` was called.
    pub(crate) fn config_subscriptions(&self) -> usize {
        *self
            .config_subscribed
            .lock()
            .expect("config subscribed lock")
    }

    fn set_status(&self, sheep: &str, status: ProcStatus) {
        self.flock
            .lock()
            .expect("flock lock")
            .insert(sheep.to_owned(), status);
    }

    /// How many times `process_events` was called.
    pub(crate) fn subscriptions(&self) -> usize {
        *self.subscribed.lock().expect("subscribed lock")
    }

    fn record(&self, call: Call) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

impl Shepherd for FakeShepherd {
    async fn dog_config(&self, _name: &str) -> Result<String, ShepherdError> {
        *self.section_reads.lock().expect("section reads lock") += 1;
        Ok(self.section.lock().expect("section lock").clone())
    }

    async fn list_flock(&self) -> Result<Vec<ProcessInfo>, ShepherdError> {
        *self.listings.lock().expect("listings lock") += 1;
        let flock = self.flock.lock().expect("flock lock");
        Ok(flock
            .iter()
            .zip(1..)
            .map(|((sheep, status), id)| {
                ProcessInfo::builder(id, sheep.as_str(), *status)
                    .pid(self.pid_of(sheep))
                    .build()
            })
            .collect())
    }

    async fn set_field(
        &self,
        sheep: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), ShepherdError> {
        self.record(Call::SetField(sheep.to_owned(), key.to_owned(), value));
        Ok(())
    }

    async fn set_env(&self, sheep: &str, key: &str, value: &str) -> Result<(), ShepherdError> {
        self.record(Call::SetEnv(
            sheep.to_owned(),
            key.to_owned(),
            value.to_owned(),
        ));
        Ok(())
    }

    async fn restart(&self, sheep: &str) -> Result<Option<u32>, ShepherdError> {
        self.record(Call::Restart(sheep.to_owned()));
        if self.stall_restart {
            self.set_status(sheep, ProcStatus::Starting);
            core::future::pending::<()>().await;
        }
        if let Some(gate) = &self.gate {
            self.set_status(sheep, ProcStatus::Starting);
            if let Ok(permit) = gate.acquire().await {
                permit.forget();
            }
        }
        match &self.refuse_restart {
            Some(what) => Err(ShepherdError::Refused { what: what.clone() }),
            None if self.start_without_online => {
                let pid = self.spawn(sheep);
                self.set_status(sheep, ProcStatus::Starting);
                Ok(Some(pid))
            }
            None => {
                let pid = self.spawn(sheep);
                self.come_online(sheep);
                Ok(Some(pid))
            }
        }
    }

    async fn stop(&self, sheep: &str) -> Result<(), ShepherdError> {
        self.record(Call::Stop(sheep.to_owned()));
        self.set_status(sheep, ProcStatus::Stopped);
        let mut failing = self.failing_stops.lock().expect("failing stops lock");
        if *failing > 0 {
            *failing -= 1;
            return Err(ShepherdError::Refused {
                what: format!("{sheep}: stop failed"),
            });
        }
        Ok(())
    }

    async fn process_events(&self) -> Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError> {
        *self.subscribed.lock().expect("subscribed lock") += 1;
        let feed = match self.feeds.lock().expect("feeds lock").pop_front() {
            Some(Some(feed)) => feed,
            Some(None) => {
                return Err(ShepherdError::Unexpected {
                    what: "a refused subscription",
                });
            }
            None => return Ok(stream::pending().boxed_local()),
        };
        Ok(stream::unfold(feed, |mut feed| async move {
            feed.recv().await.map(|event| (event, feed))
        })
        .boxed_local())
    }

    async fn sheep_online(&self) -> Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError> {
        let (tx, rx) = unbounded_channel();
        self.online.lock().expect("online lock").push(tx);
        Ok(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed_local())
    }

    async fn config_changes(
        &self,
        _dog: &str,
    ) -> Result<LocalBoxStream<'static, ()>, ShepherdError> {
        *self
            .config_subscribed
            .lock()
            .expect("config subscribed lock") += 1;
        let feed = match self
            .config_feeds
            .lock()
            .expect("config feeds lock")
            .pop_front()
        {
            Some(Some(feed)) => feed,
            Some(None) => {
                return Err(ShepherdError::Unexpected {
                    what: "a refused subscription",
                });
            }
            None => return Ok(stream::pending().boxed_local()),
        };
        Ok(stream::unfold(feed, |mut feed| async move {
            feed.recv().await.map(|()| ((), feed))
        })
        .boxed_local())
    }
}
