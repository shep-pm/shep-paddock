//! What the dog logs when a subscription to the shepherd ends and opens again.

use core::fmt::Display;
use std::time::Duration;

use tokio::time::Instant;

/// How long after a subscription ended a failed subscribe goes unlogged
///
/// A `shep daemon reload` closes every connection and its first subscribe races the handover.
pub(crate) const QUIET: Duration = Duration::from_secs(5);

/// One subscription's ends since it last opened, and the lines to log for them
#[derive(Debug)]
pub(crate) struct Outage {
    stream: &'static str,
    ended: Option<Instant>,
}

impl Outage {
    /// A subscription to `stream` not yet opened, which logs nothing until it ends
    pub(crate) fn new(stream: &'static str) -> Self {
        Outage {
            stream,
            ended: None,
        }
    }

    /// The subscription ended at `now`; returns the line to log
    pub(crate) fn ended(&mut self, now: Instant) -> String {
        self.ended = Some(now);
        format!(
            "paddock: {} ended; the connection to the shepherd closed, as a `shep daemon reload` does; subscribing again",
            self.stream
        )
    }

    /// A subscribe failed at `now`; returns the line to log, if any
    pub(crate) fn failed(&self, now: Instant, err: &impl Display) -> Option<String> {
        let since_end = self.ended.map(|ended| now.saturating_duration_since(ended));
        failure_line(self.stream, since_end, err)
    }

    /// A subscribe succeeded at `now`; returns the line to log, if any
    pub(crate) fn opened(&mut self, now: Instant) -> Option<String> {
        let ended = self.ended.take()?;
        Some(rejoin_line(
            self.stream,
            now.saturating_duration_since(ended),
        ))
    }
}

/// The line for a failed subscribe `since_end` after the last end, or `None` for one at start-up
/// or inside [`QUIET`]
fn failure_line(stream: &str, since_end: Option<Duration>, err: &impl Display) -> Option<String> {
    since_end
        .is_none_or(|since| since >= QUIET)
        .then(|| format!("paddock: subscribing to {stream} failed: {err}"))
}

fn rejoin_line(stream: &str, since_end: Duration) -> String {
    format!(
        "paddock: rejoined the shepherd after {:.1} s; {stream} resumed",
        since_end.as_secs_f64()
    )
}

#[cfg(test)]
mod tests;
