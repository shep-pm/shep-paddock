//! The body of `POST /paddock/leases`, read into what the engine is asked for, and the 400s it can earn.

use core::fmt;
use std::time::Duration;

use hyper::{Response, StatusCode};
use serde::Deserialize;
use serde_json::json;
use shep_client::shep_core::values::UpDuration;

use super::duration_text;
use crate::{
    book::{Hold, Priority},
    config::ModelName,
    engine::LeaseRequest,
    http::{Body, reply},
};

// The spec's default for a heartbeat lease.
const DEFAULT_TTL: Duration = Duration::from_secs(60);
// A heartbeat holder that vanishes keeps its model held for at most one ttl.
const MAX_TTL: Duration = Duration::from_secs(60 * 60);
// A note is a label for status and `state.json`, so a long one is a mistake.
pub(super) const MAX_NOTE: usize = 1024;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PriorityText {
    Interactive,
    Batch,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HoldText {
    Connection,
    Heartbeat,
}

/// The body of `POST /paddock/leases`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Take {
    model: String,
    priority: Option<PriorityText>,
    expected: Option<String>,
    note: Option<String>,
    hold: Option<HoldText>,
    ttl: Option<String>,
    max_wait: Option<String>,
    release_if_idle: Option<String>,
    reclaimable: Option<bool>,
}

/// The body of a `PUT`, which renews without a `note` and records one with it
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Note {
    pub(super) note: Option<String>,
}

/// Why a take or a note was answered with a `400`
#[derive(Debug)]
pub(super) enum BadTake {
    /// The body is not the shape above.
    Body(String),
    /// A duration is not in shep's `UpDuration` grammar.
    Duration(&'static str),
    /// A heartbeat lease's `ttl` is longer than [`MAX_TTL`].
    TtlTooLong,
    /// `note` is longer than [`MAX_NOTE`] bytes.
    NoteTooLong,
    /// `release_if_idle` is 0, which would end the lease at its grant.
    IdleZero,
}

impl BadTake {
    /// The `error` the `400` names
    fn code(&self) -> &'static str {
        match self {
            Self::Body(_) | Self::Duration(_) | Self::IdleZero => "bad_lease_request",
            Self::TtlTooLong => "bad_ttl",
            Self::NoteTooLong => "note_too_long",
        }
    }
}

impl fmt::Display for BadTake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(why) => f.write_str(why),
            Self::Duration(field) => write!(f, "{field} is not a duration such as 30s or 8h"),
            Self::TtlTooLong => write!(f, "ttl is at most {}", duration_text(MAX_TTL)),
            Self::NoteTooLong => write!(f, "note is at most {MAX_NOTE} bytes"),
            Self::IdleZero => f.write_str("release_if_idle must be more than 0"),
        }
    }
}

impl core::error::Error for BadTake {}

fn duration(field: &'static str, text: Option<&str>) -> Result<Option<Duration>, BadTake> {
    text.map(|text| {
        text.parse::<UpDuration>()
            .map(UpDuration::as_duration)
            .map_err(|_| BadTake::Duration(field))
    })
    .transpose()
}

impl Take {
    pub(super) fn parse(body: &[u8]) -> Result<Self, BadTake> {
        serde_json::from_slice(body).map_err(|err| BadTake::Body(err.to_string()))
    }

    /// What to ask the engine for, and the `ttl` a heartbeat lease will be told
    pub(super) fn request(self) -> Result<(LeaseRequest, Duration), BadTake> {
        let ttl = duration("ttl", self.ttl.as_deref())?.unwrap_or(DEFAULT_TTL);
        if self.note.as_ref().is_some_and(|note| note.len() > MAX_NOTE) {
            return Err(BadTake::NoteTooLong);
        }
        let hold = match self.hold {
            None | Some(HoldText::Connection) => Hold::Connection,
            Some(HoldText::Heartbeat) if ttl > MAX_TTL => return Err(BadTake::TtlTooLong),
            Some(HoldText::Heartbeat) => Hold::Heartbeat { ttl },
        };
        let release_if_idle = duration("release_if_idle", self.release_if_idle.as_deref())?;
        if release_if_idle == Some(Duration::ZERO) {
            return Err(BadTake::IdleZero);
        }
        let priority = match self.priority {
            Some(PriorityText::Interactive) => Priority::Interactive,
            None | Some(PriorityText::Batch) => Priority::Batch,
        };
        let request = LeaseRequest {
            model: ModelName::from(self.model),
            priority,
            expected: duration("expected", self.expected.as_deref())?,
            max_wait: duration("max_wait", self.max_wait.as_deref())?,
            hold,
            note: self.note,
            reclaimable: self.reclaimable.unwrap_or(false),
            release_if_idle,
        };
        Ok((request, ttl))
    }
}

pub(super) fn bad_take(bad: &BadTake) -> Response<Body> {
    reply::json(
        StatusCode::BAD_REQUEST,
        json!({ "error": bad.code(), "detail": bad.to_string() }),
    )
}
