//! What the dog takes from the shepherd's event bus: each sheep's lifecycle events, and which
//! of them each subscriber keeps.

use shep_client::{
    Lagged,
    shep_core::protocol::{BusEvent, ProcessEventKind},
};

use super::fan::{Fan, Pick};

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
    /// The process's pid, when the shepherd names one.
    pub pid: Option<u32>,
}

/// The process event a bus item carries, if it carries one. A lagged notice carries none.
pub(super) fn process_event(item: Result<BusEvent, Lagged>) -> Option<ProcessEvent> {
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
        pid: info.pid,
    })
}

/// What the engine's stream takes from the shared subscription
///
/// A lag ends the stream: an event was dropped and it may have been an exit, so the engine
/// rejoins and reconciles against the flock rather than trust what it has.
pub(super) fn process_pick(fan: Fan) -> Pick<ProcessEvent> {
    match fan {
        Fan::Process(event) => Pick::Keep(event),
        Fan::Config(_) => Pick::Skip,
        Fan::Lagged => Pick::End,
    }
}

/// What a load waiting for its sheep takes from the shared subscription
///
/// A lag ends the stream, as for [`process_pick`]: the `online` may have been dropped.
pub(super) fn online_pick(fan: Fan) -> Pick<ProcessEvent> {
    match fan {
        Fan::Process(event) if event.kind == ProcessKind::Online => Pick::Keep(event),
        Fan::Process(_) | Fan::Config(_) => Pick::Skip,
        Fan::Lagged => Pick::End,
    }
}

/// What the config watcher's stream takes from the shared subscription
///
/// A lag counts as a change: an event was dropped, and it may have been the change.
pub(super) fn config_pick(dog: String) -> impl Fn(Fan) -> Pick<()> {
    move |fan| match fan {
        Fan::Config(named) if named == dog => Pick::Keep(()),
        Fan::Lagged => Pick::Keep(()),
        Fan::Config(_) | Fan::Process(_) => Pick::Skip,
    }
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{protocol::ProcessInfo, status::ProcStatus};

    use super::*;

    fn bus(event: ProcessEventKind, manually: bool) -> Result<BusEvent, Lagged> {
        Ok(BusEvent::Process {
            event,
            info: ProcessInfo::builder(1, "iq3_s", ProcStatus::Stopped)
                .pid(Some(42))
                .build(),
            manually,
            at_ms: 0,
        })
    }

    fn kind_of(event: ProcessEventKind) -> Option<ProcessKind> {
        process_event(bus(event, false)).map(|seen| seen.kind)
    }

    #[test]
    fn process_events_keep_the_sheep_name_pid_and_who_caused_them() {
        assert_eq!(
            process_event(bus(ProcessEventKind::Stop, true)),
            Some(ProcessEvent {
                sheep: "iq3_s".to_owned(),
                kind: ProcessKind::Stop,
                manually: true,
                pid: Some(42),
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
}
