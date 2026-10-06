//! The lease stream reader: the dog's NDJSON lines as events, and the request that opens them.

use core::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::Value;
use shep_client::shep_core::values::UpDuration;
use tokio::time::{Instant, timeout, timeout_at};

use crate::cli::Link;

// Used when the dog's `reconnect` is missing or not a duration; the spec's default.
const RECONNECT: Duration = Duration::from_secs(60);

/// What the dog says on a lease's stream
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    Queued {
        reason: String,
    },
    Granted {
        id: String,
        reconnect: Duration,
    },
    Heartbeat,
    Ended {
        reason: String,
        /// How long the lease sat unused, when `reason` is `idle`.
        idle_for: Option<String>,
    },
    Refused {
        reason: String,
        expected_until: Option<String>,
    },
    Failed {
        reason: String,
    },
    Unknown,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn parse_event(line: &[u8]) -> Option<Event> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let reason = |what: &str| text(&value[what], "reason").unwrap_or_default();
    Some(if let Some(queued) = value.get("queued") {
        Event::Queued {
            reason: text(queued, "reason").unwrap_or_default(),
        }
    } else if let Some(granted) = value.get("granted") {
        Event::Granted {
            id: text(granted, "id")?,
            reconnect: text(granted, "reconnect")
                .and_then(|text| text.parse::<UpDuration>().ok())
                .map_or(RECONNECT, UpDuration::as_duration),
        }
    } else if value.get("heartbeat").is_some() {
        Event::Heartbeat
    } else if let Some(ended) = value.get("ended") {
        Event::Ended {
            reason: text(ended, "reason").unwrap_or_default(),
            idle_for: text(ended, "idle_for"),
        }
    } else if let Some(refused) = value.get("refused") {
        Event::Refused {
            reason: reason("refused"),
            expected_until: text(refused, "expected_until"),
        }
    } else if value.get("failed").is_some() {
        Event::Failed {
            reason: reason("failed"),
        }
    } else {
        Event::Unknown
    })
}

/// A lease's NDJSON stream
pub(super) struct Stream {
    response: reqwest::Response,
    buffer: Vec<u8>,
    /// When the stream counts as broken unless bytes arrive first.
    deadline: Instant,
}

pub(super) enum Next {
    Event(Event),
    /// The connection ended or went quiet, whatever the dog had said.
    Broken,
}

impl Stream {
    fn line(&mut self) -> Option<Vec<u8>> {
        let end = self.buffer.iter().position(|byte| *byte == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
        line.pop();
        Some(line)
    }

    /// The next event, skipping lines that are not one
    ///
    /// # Cancellation safety
    /// Safe: what has been read stays in the buffer, and the deadline stays where the last bytes
    /// put it.
    pub(super) async fn next(&mut self, silence: Duration) -> Next {
        loop {
            if let Some(line) = self.line() {
                match parse_event(&line) {
                    Some(event) => return Next::Event(event),
                    None => continue,
                }
            }
            match timeout_at(self.deadline, self.response.chunk()).await {
                Ok(Ok(Some(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                    self.deadline = Instant::now() + silence;
                }
                _ => return Next::Broken,
            }
        }
    }
}

/// Why a request to the dog did not open a stream
pub(super) enum Rejected {
    /// The dog could not be reached.
    Unreachable(reqwest::Error),
    /// The dog took the connection and did not answer within the silence limit.
    Silent(Duration),
    /// The dog answered with a status outside 2xx, and this body.
    Status(StatusCode, String),
}

impl core::fmt::Display for Rejected {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreachable(err) => write!(f, "{err}"),
            Self::Silent(wait) => write!(f, "the dog did not answer within {wait:?}"),
            Self::Status(code, body) => write!(f, "the dog answered {code}: {body}"),
        }
    }
}

/// Opens a stream, giving up on a dog that stays silent for `link.silence`
pub(super) async fn open(
    client: &reqwest::Client,
    link: &Link,
    path: &str,
    body: Option<Value>,
) -> Result<Stream, Rejected> {
    let mut request = link.request(client, Method::POST, path);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    // Not `RequestBuilder::timeout`, which would also cut the stream that follows.
    let response = timeout(link.silence, request.send())
        .await
        .map_err(|_| Rejected::Silent(link.silence))?
        .map_err(Rejected::Unreachable)?;
    if !response.status().is_success() {
        let code = response.status();
        let body = timeout(link.silence, response.text())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        return Err(Rejected::Status(code, body));
    }
    Ok(Stream {
        response,
        buffer: Vec::new(),
        deadline: Instant::now() + link.silence,
    })
}
