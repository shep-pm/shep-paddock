//! The saved state: the leases and which model each sheep was last started for, kept across a restart
//!
//! The engine writes it to `$SHEP_HOME/paddock/state.json` after every lease
//! change and every load. At start, [`load_or_empty`] reads it back. A file
//! that cannot be read as version 1 is logged and set aside, so the dog starts
//! with no leases rather than failing on every restart shep gives it.

use core::fmt;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use shep_client::shep_core::atomic_file;

use crate::{
    book::{Hold, LeaseAsk, LeaseId, LeaseView, Priority, RestoredLease},
    config::{ClientName, ModelName},
    engine::Clock,
};

/// The only version this dog reads and writes.
pub(crate) const VERSION: u32 = 1;

/// Everything the dog keeps across a restart
// wire format: state.json, so changing this is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Saved {
    /// Which shape the rest of the file has.
    pub version: u32,
    /// Every granted lease, by id.
    pub leases: Vec<SavedLease>,
    /// The model each sheep was last started for.
    pub sheep: BTreeMap<String, ModelName>,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            version: VERSION,
            leases: Vec::new(),
            sheep: BTreeMap::new(),
        }
    }
}

/// One granted lease, as it is written to disk
// wire format: state.json, so changing this is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedLease {
    /// The lease.
    pub id: LeaseId,
    /// Who holds it.
    pub client: ClientName,
    /// The model it holds.
    pub model: ModelName,
    /// Where it queued.
    pub priority: Priority,
    /// When it was granted.
    pub since: jiff::Timestamp,
    /// When its holder expects to release it, if it said.
    pub expected_until: Option<jiff::Timestamp>,
    /// What the holder says it is for.
    pub note: Option<String>,
    /// How its holder shows it is still alive.
    pub hold: SavedHold,
}

/// `Hold` as it is written to disk: `{"connection": {}}` or `{"heartbeat": {"ttl_ms": 60000}}`.
// wire format: state.json, so changing this is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SavedHold {
    /// The holder keeps a stream open.
    Connection {},
    /// The holder renews within every `ttl_ms` milliseconds.
    Heartbeat {
        /// How long a renewal lasts, in milliseconds.
        ttl_ms: u64,
    },
}

impl From<Hold> for SavedHold {
    fn from(hold: Hold) -> Self {
        match hold {
            Hold::Connection => Self::Connection {},
            Hold::Heartbeat { ttl } => Self::Heartbeat {
                ttl_ms: u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            },
        }
    }
}

impl From<SavedHold> for Hold {
    fn from(hold: SavedHold) -> Self {
        match hold {
            SavedHold::Connection {} => Self::Connection,
            SavedHold::Heartbeat { ttl_ms } => Self::Heartbeat {
                ttl: Duration::from_millis(ttl_ms),
            },
        }
    }
}

impl SavedLease {
    /// The lease `view` shows, with its moments as wall-clock times
    pub fn from_view(view: LeaseView, clock: &Clock) -> Self {
        Self {
            id: view.id,
            client: view.client,
            model: view.model,
            priority: view.priority,
            since: clock.wall(view.since),
            expected_until: view.expected_until.map(|until| clock.wall(until)),
            note: view.note,
            hold: view.hold.into(),
        }
    }

    /// The lease for the book to pick up, with its times as `clock`'s moments
    ///
    /// The expected length runs between the two moments, so a grant older
    /// than the clock reaches still ends when its holder said.
    pub fn restored(self, clock: &Clock) -> RestoredLease {
        let since = clock.moment_of(self.since);
        let expected = self
            .expected_until
            .map(|until| clock.moment_of(until).since(since));
        RestoredLease {
            ask: LeaseAsk {
                lease: self.id,
                client: self.client,
                model: self.model,
                priority: self.priority,
                expected,
                max_wait: None,
                hold: self.hold.into(),
                note: self.note,
            },
            since,
        }
    }
}

/// Why the saved state could not be read or written
#[derive(Debug)]
pub(crate) enum SavedError {
    /// The file exists and could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the read reported.
        source: std::io::Error,
    },
    /// The file is not JSON, or not the shape its version has.
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What the parser reported.
        reason: String,
    },
    /// The file names a version other than [`VERSION`].
    Version {
        /// The file.
        path: PathBuf,
        /// The version it names.
        found: u64,
    },
    /// The file or its directory could not be written.
    Write {
        /// The file.
        path: PathBuf,
        /// What the write reported.
        source: std::io::Error,
    },
}

impl fmt::Display for SavedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "reading {} failed: {source}", path.display())
            }
            Self::Corrupt { path, reason } => {
                write!(f, "{} is not valid saved state: {reason}", path.display())
            }
            Self::Version { path, found } => write!(
                f,
                "{} is version {found}, and this dog reads only version {VERSION}",
                path.display()
            ),
            Self::Write { path, source } => {
                write!(f, "writing {} failed: {source}", path.display())
            }
        }
    }
}

impl core::error::Error for SavedError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Corrupt { .. } | Self::Version { .. } => None,
        }
    }
}

/// Where the saved state lives under `$SHEP_HOME`
pub(crate) fn path_in(shep_home: &Path) -> PathBuf {
    shep_home.join("paddock").join("state.json")
}

/// The saved state at `path`, or `None` when there is no file
///
/// # Errors
/// [`SavedError::Read`] when the file exists and cannot be read,
/// [`SavedError::Corrupt`] when it is not version 1's JSON, and
/// [`SavedError::Version`] when it names another version.
pub(crate) fn load(path: &Path) -> Result<Option<Saved>, SavedError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SavedError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let corrupt = |err: serde_json::Error| SavedError::Corrupt {
        path: path.to_owned(),
        reason: err.to_string(),
    };
    let header: Header = serde_json::from_slice(&bytes).map_err(corrupt)?;
    if header.version != u64::from(VERSION) {
        return Err(SavedError::Version {
            path: path.to_owned(),
            found: header.version,
        });
    }
    serde_json::from_slice(&bytes).map(Some).map_err(corrupt)
}

/// Only the version, read before the rest, so a newer file is told apart from a corrupt one.
#[derive(Deserialize)]
struct Header {
    version: u64,
}

/// The saved state at `path`, or an empty one after a line to `log` saying why
///
/// A missing file is a first start and logs nothing.
pub(crate) fn load_or_empty(path: &Path, log: &mut impl Write) -> Saved {
    match load(path) {
        Ok(saved) => saved.unwrap_or_default(),
        Err(err) => {
            // Nothing is left to tell if the log itself cannot be written.
            let _ = writeln!(log, "paddock: {err}; starting with no saved leases");
            Saved::default()
        }
    }
}

/// Replaces the file at `path` with `saved`, creating its directory if needed
///
/// # Errors
/// [`SavedError::Write`] when the directory, the staging file or the
/// rename over `path` fails. `path` keeps its old contents then.
pub(crate) fn store(path: &Path, saved: &Saved) -> Result<(), SavedError> {
    let failed = |source| SavedError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    atomic_file::write_json(path, "state", saved).map_err(failed)
}

#[cfg(test)]
mod tests;
