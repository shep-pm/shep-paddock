//! The shepherd as the backends see it: the few requests they make, behind a trait so the
//! orchestration is testable without a daemon.
//!
//! One real implementation, [`Live`], and one fake in `test_support`. The `async fn`s are used
//! through generic bounds only, never behind `dyn`.

use core::fmt;
use std::{rc::Rc, sync::Arc};

use futures_util::stream::LocalBoxStream;
use shep_client::{
    Lagged, ReconnectingClient, RequestError,
    shep_core::{
        protocol::{
            BusEvent, EnvValue, ProcessEventKind, ProcessInfo, Request, Response, SelectorSpec,
        },
        status::ProcStatus,
    },
};
use tokio::sync::broadcast;

use fan::{Fan, Hub, Pick};

mod fan;

/// The topic carrying every sheep's lifecycle events.
const PROCESS_TOPIC: &str = "process.*";

/// The topic carrying every dog's config changes.
const CONFIG_TOPIC: &str = "config.dog.*";

/// Why a request to the shepherd did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShepherdError {
    /// The request failed on the wire or the shepherd answered it with an error.
    Request(RequestError),
    /// The shepherd answered, and named sheep it refused to restart or reload.
    Refused {
        /// Each refused sheep and the shepherd's reason.
        what: String,
    },
    /// The shepherd answered with a response this request never gets.
    Unexpected {
        /// The name of the response variant that arrived.
        what: &'static str,
    },
}

impl fmt::Display for ShepherdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(err) => write!(f, "shepherd request failed: {err}"),
            Self::Refused { what } => write!(f, "shepherd refused: {what}"),
            Self::Unexpected { what } => write!(f, "shepherd answered with an unexpected {what}"),
        }
    }
}

impl core::error::Error for ShepherdError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Request(err) => Some(err),
            Self::Refused { .. } | Self::Unexpected { .. } => None,
        }
    }
}

impl From<RequestError> for ShepherdError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// What happened to a sheep, in the terms the engine acts on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessKind {
    /// The process exited and the shepherd will start it again.
    Exit,
    /// The process exited and used up its restart budget.
    Errored,
    /// The process stopped and stays stopped: asked to, or exited with no restart to come.
    Stop,
    /// A new process began: a start or a restart.
    Started,
    /// A started process passed its probe or its `listen_timeout`.
    Online,
    /// Anything else, such as a reload or a delete.
    Other,
}

/// One sheep's lifecycle event
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessEvent {
    /// The sheep's name.
    pub sheep: String,
    /// What happened.
    pub kind: ProcessKind,
    /// Whether a request to the shepherd caused it, rather than the process itself.
    pub manually: bool,
}

/// The requests the dog makes of the shepherd.
pub(crate) trait Shepherd {
    /// The dog's own section of `dogs.toml`, as TOML text.
    ///
    /// # Errors
    /// [`ShepherdError::Request`] if the shepherd cannot be reached or refuses, and
    /// [`ShepherdError::Unexpected`] if it answers with anything but a dog section.
    async fn dog_config(&self, name: &str) -> Result<String, ShepherdError>;

    /// Every supervised entry, dogs included.
    ///
    /// # Errors
    /// As [`Self::dog_config`], with a flock listing as the expected answer.
    async fn list_flock(&self) -> Result<Vec<ProcessInfo>, ShepherdError>;

    /// Parks one config field on a sheep.
    ///
    /// # Errors
    /// As [`Self::dog_config`], with a field-set acknowledgement as the expected answer.
    async fn set_field(
        &self,
        sheep: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), ShepherdError>;

    /// Parks one env key on a sheep.
    ///
    /// # Errors
    /// As [`Self::dog_config`], with an env-set acknowledgement as the expected answer.
    async fn set_env(&self, sheep: &str, key: &str, value: &str) -> Result<(), ShepherdError>;

    /// Restarts a sheep, promoting its parked fields whether it is running or stopped.
    ///
    /// # Errors
    /// As [`Self::dog_config`], plus [`ShepherdError::Refused`] when the shepherd names the
    /// sheep as refused or reports it errored.
    async fn restart(&self, sheep: &str) -> Result<(), ShepherdError>;

    /// Stops a sheep.
    ///
    /// # Errors
    /// As [`Self::dog_config`], with a stop acknowledgement as the expected answer.
    async fn stop(&self, sheep: &str) -> Result<(), ShepherdError>;

    /// Subscribes to every `process.*` event. The stream ends with its connection.
    ///
    /// # Errors
    /// [`ShepherdError::Request`] if the subscription is not accepted.
    async fn process_events(&self) -> Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError>;

    /// Subscribes to the name of each sheep as it comes online. The stream
    /// ends with its connection, or when an event may have been dropped.
    ///
    /// # Errors
    /// [`ShepherdError::Request`] if the subscription is not accepted.
    async fn sheep_online(&self) -> Result<LocalBoxStream<'static, String>, ShepherdError>;

    /// Subscribes to changes of the dog's own section. Each item means the section may have
    /// changed, so it is read again. The stream ends with its connection.
    ///
    /// # Errors
    /// [`ShepherdError::Request`] if the subscription is not accepted.
    async fn config_changes(&self, dog: &str)
    -> Result<LocalBoxStream<'static, ()>, ShepherdError>;
}

/// The shepherd over its control socket.
///
/// `Debug` is written, not derived, so the socket path never reaches a log.
#[derive(Clone)]
pub(crate) struct Live {
    // Shared so the config watcher and the engine each hold the one connection.
    client: Arc<ReconnectingClient>,
    // The one subscription, which `process_events` and `config_changes` both ride.
    hub: Rc<Hub>,
}

impl Live {
    /// Wraps a connected client.
    pub(crate) fn new(client: ReconnectingClient) -> Self {
        Self {
            client: Arc::new(client),
            hub: Rc::new(Hub::default()),
        }
    }

    /// A receiver on the shared subscription, which carries every sheep's events and every
    /// dog's config changes. Subscribing twice on one connection would replace the first.
    async fn join(&self) -> Result<broadcast::Receiver<Fan>, ShepherdError> {
        let topics = vec![PROCESS_TOPIC.to_owned(), CONFIG_TOPIC.to_owned()];
        self.hub
            .join(async { Ok(self.client.subscribe(topics).await?) })
            .await
    }
}

impl fmt::Debug for Live {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Live(<shepherd session>)")
    }
}

fn unexpected(got: &Response) -> ShepherdError {
    ShepherdError::Unexpected { what: got.name() }
}

/// What a restart's answer means: refused names and rows that errored are failures.
fn restart_outcome(response: Response) -> Result<(), ShepherdError> {
    match response {
        Response::Restarted { accepted, refused } => {
            let mut what: Vec<String> = refused
                .iter()
                .map(|r| format!("{}: {}", r.name, r.reason))
                .collect();
            what.extend(
                accepted
                    .iter()
                    .filter(|row| row.status == ProcStatus::Errored)
                    .map(|row| format!("{}: errored on restart", row.name)),
            );
            if what.is_empty() {
                Ok(())
            } else {
                Err(ShepherdError::Refused {
                    what: what.join("; "),
                })
            }
        }
        other => Err(unexpected(&other)),
    }
}

/// The process event a bus item carries, if it carries one. A lagged notice carries none.
fn process_event(item: Result<BusEvent, Lagged>) -> Option<ProcessEvent> {
    let Ok(BusEvent::Process {
        event,
        info,
        manually,
        ..
    }) = item
    else {
        return None;
    };
    let kind = match event {
        ProcessEventKind::Exit => ProcessKind::Exit,
        ProcessEventKind::Errored => ProcessKind::Errored,
        ProcessEventKind::Stop => ProcessKind::Stop,
        ProcessEventKind::Start | ProcessEventKind::Restart => ProcessKind::Started,
        ProcessEventKind::Online => ProcessKind::Online,
        _ => ProcessKind::Other,
    };
    Some(ProcessEvent {
        sheep: info.name,
        kind,
        manually,
    })
}

impl Shepherd for Live {
    async fn dog_config(&self, name: &str) -> Result<String, ShepherdError> {
        let asked = Request::DogConfig {
            name: name.to_owned(),
        };
        match self.client.request(asked).await? {
            Response::DogSection { toml } => Ok(toml.as_str().to_owned()),
            other => Err(unexpected(&other)),
        }
    }

    async fn list_flock(&self) -> Result<Vec<ProcessInfo>, ShepherdError> {
        match self.client.request(Request::ListFlock).await? {
            Response::Flock(flock) => Ok(flock),
            other => Err(unexpected(&other)),
        }
    }

    async fn set_field(
        &self,
        sheep: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), ShepherdError> {
        let asked = Request::SetSheepField {
            name: sheep.to_owned(),
            key: key.to_owned(),
            value,
        };
        match self.client.request(asked).await? {
            Response::SheepFieldSet { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    async fn set_env(&self, sheep: &str, key: &str, value: &str) -> Result<(), ShepherdError> {
        let asked = Request::SetSheepEnv {
            name: sheep.to_owned(),
            key: key.to_owned(),
            value: Some(EnvValue::from(value.to_owned())),
        };
        match self.client.request(asked).await? {
            Response::SheepEnvSet { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    async fn restart(&self, sheep: &str) -> Result<(), ShepherdError> {
        let asked = Request::Restart {
            selector: SelectorSpec::Name(sheep.to_owned()),
        };
        restart_outcome(self.client.request(asked).await?)
    }

    async fn stop(&self, sheep: &str) -> Result<(), ShepherdError> {
        let asked = Request::Stop {
            selector: SelectorSpec::Name(sheep.to_owned()),
        };
        match self.client.request(asked).await? {
            Response::Stopped(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    async fn process_events(&self) -> Result<LocalBoxStream<'static, ProcessEvent>, ShepherdError> {
        let events = self.join().await?;
        Ok(fan::consume(events, process_pick))
    }

    async fn sheep_online(&self) -> Result<LocalBoxStream<'static, String>, ShepherdError> {
        let events = self.join().await?;
        Ok(fan::consume(events, online_pick))
    }

    async fn config_changes(
        &self,
        dog: &str,
    ) -> Result<LocalBoxStream<'static, ()>, ShepherdError> {
        let events = self.join().await?;
        Ok(fan::consume(events, config_pick(dog.to_owned())))
    }
}

/// What the engine's stream takes from the shared subscription
///
/// A lag ends the stream: an event was dropped and it may have been an exit, so the engine
/// rejoins and reconciles against the flock rather than trust what it has.
fn process_pick(fan: Fan) -> Pick<ProcessEvent> {
    match fan {
        Fan::Process(event) => Pick::Keep(event),
        Fan::Config(_) => Pick::Skip,
        Fan::Lagged => Pick::End,
    }
}

/// What a load waiting for its sheep takes from the shared subscription
///
/// A lag ends the stream, as for [`process_pick`]: the `online` may have been dropped.
fn online_pick(fan: Fan) -> Pick<String> {
    match fan {
        Fan::Process(event) if event.kind == ProcessKind::Online => Pick::Keep(event.sheep),
        Fan::Process(_) | Fan::Config(_) => Pick::Skip,
        Fan::Lagged => Pick::End,
    }
}

/// What the config watcher's stream takes from the shared subscription
///
/// A lag counts as a change: an event was dropped, and it may have been the change.
fn config_pick(dog: String) -> impl Fn(Fan) -> Pick<()> {
    move |fan| match fan {
        Fan::Config(named) if named == dog => Pick::Keep(()),
        Fan::Lagged => Pick::Keep(()),
        Fan::Config(_) | Fan::Process(_) => Pick::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_client::shep_core::protocol::SheepRefusal;

    fn row(status: ProcStatus) -> ProcessInfo {
        ProcessInfo::builder(1, "iq3_s", status).build()
    }

    #[test]
    fn a_restart_with_a_refused_name_is_refused() {
        let answer = Response::Restarted {
            accepted: Vec::new(),
            refused: vec![SheepRefusal::new("iq3_s", "no such sheep")],
        };
        assert_eq!(
            restart_outcome(answer),
            Err(ShepherdError::Refused {
                what: "iq3_s: no such sheep".to_owned()
            })
        );
    }

    #[test]
    fn a_restart_with_an_errored_row_is_refused() {
        let answer = Response::Restarted {
            accepted: vec![row(ProcStatus::Errored)],
            refused: Vec::new(),
        };
        assert_eq!(
            restart_outcome(answer),
            Err(ShepherdError::Refused {
                what: "iq3_s: errored on restart".to_owned()
            })
        );
    }

    #[test]
    fn a_restart_with_online_rows_is_accepted() {
        let answer = Response::Restarted {
            accepted: vec![row(ProcStatus::Online)],
            refused: Vec::new(),
        };
        assert_eq!(restart_outcome(answer), Ok(()));
    }

    fn bus(event: ProcessEventKind, manually: bool) -> Result<BusEvent, Lagged> {
        Ok(BusEvent::Process {
            event,
            info: row(ProcStatus::Stopped),
            manually,
            at_ms: 0,
        })
    }

    fn kind_of(event: ProcessEventKind) -> Option<ProcessKind> {
        process_event(bus(event, false)).map(|seen| seen.kind)
    }

    #[test]
    fn process_events_keep_the_sheep_name_and_who_caused_them() {
        assert_eq!(
            process_event(bus(ProcessEventKind::Stop, true)),
            Some(ProcessEvent {
                sheep: "iq3_s".to_owned(),
                kind: ProcessKind::Stop,
                manually: true,
            })
        );
    }

    #[test]
    fn each_process_event_kind_maps_to_what_the_engine_acts_on() {
        use ProcessEventKind as Bus;
        assert_eq!(kind_of(Bus::Exit), Some(ProcessKind::Exit));
        assert_eq!(kind_of(Bus::Errored), Some(ProcessKind::Errored));
        assert_eq!(kind_of(Bus::Stop), Some(ProcessKind::Stop));
        for started in [Bus::Start, Bus::Restart] {
            assert_eq!(kind_of(started), Some(ProcessKind::Started), "{started:?}");
        }
        assert_eq!(kind_of(Bus::Online), Some(ProcessKind::Online));
        for other in [Bus::Reload, Bus::Reloaded, Bus::Delete, Bus::Unrecognized] {
            assert_eq!(kind_of(other), Some(ProcessKind::Other), "{other:?}");
        }
    }

    #[test]
    fn lagged_notices_and_other_topics_are_not_process_events() {
        assert_eq!(process_event(Err(Lagged { count: 3 })), None);
        assert_eq!(
            process_event(Ok(BusEvent::LogOut {
                id: 1,
                line: "x".to_owned()
            })),
            None
        );
    }

    #[test]
    fn a_flock_answering_a_restart_is_unexpected() {
        assert_eq!(
            restart_outcome(Response::Flock(Vec::new())),
            Err(ShepherdError::Unexpected { what: "Flock" })
        );
    }

    /// A derived `Debug` would print the client, and with it the socket path.
    /// Tested against a real client because only a real one tells the two apart.
    #[tokio::test(start_paused = true)]
    async fn debug_of_live_does_not_print_the_socket_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = shep_client::testing::control_address(dir.path());
        let (_fake, _served) = shep_client::testing::fake_daemon_accepting_repeatedly(
            &socket,
            Response::Flock(Vec::new()),
        );
        let client = tokio::time::timeout(
            core::time::Duration::from_secs(5),
            ReconnectingClient::connect_as_dog(&socket, "paddock"),
        )
        .await
        .expect("connect finishes")
        .expect("connects");
        let shown = format!("{:?}", Live::new(client));

        assert_eq!(shown, "Live(<shepherd session>)");
        assert!(!shown.contains(&socket.display().to_string()));
    }
}
