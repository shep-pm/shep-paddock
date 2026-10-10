//! A lease's NDJSON stream: the body that owns the engine's receiver.

use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use hyper::body::{Body as HttpBody, Frame};
use serde_json::{Value, json};
use shep_client::shep_core::values::UpDuration;
use tokio::{
    sync::watch,
    time::{Instant, Sleep, sleep},
};

use super::{duration_text, render_id};
use crate::{
    book::{Ended, Revocation},
    config::{Config, ModelName},
    engine::{Clock, LeaseEvent, LeaseEvents},
    http::reply,
};

/// How often a stream says it is alive, so a dead peer is noticed
pub(crate) const STREAM_HEARTBEAT: Duration = Duration::from_secs(15);

/// How a lease ended, as the stream says it
fn ended_text(why: &Ended) -> &'static str {
    match why {
        Ended::Released => "released",
        Ended::Expired => "expired",
        Ended::Abandoned => "abandoned",
        Ended::Reclaimed => "reclaimed",
        Ended::Idle { .. } => "idle",
        Ended::Revoked(_) => "revoked",
    }
}

/// The line that ends a lease's stream: `{"ended": {"reason": …}}`
///
/// An idle end adds `idle_for`, the lease's `release_if_idle` in shep's duration grammar. A
/// revoked one adds `by`, the admin client, and `note`, its reason or null.
pub(crate) fn ended_line(why: &Ended) -> Value {
    match why {
        Ended::Idle { after } => {
            let millis = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
            let idle_for = UpDuration::from_millis(millis).to_string();
            json!({ "ended": { "reason": ended_text(why), "idle_for": idle_for } })
        }
        Ended::Revoked(Revocation { by, note }) => {
            json!({ "ended": { "reason": ended_text(why), "by": by.as_str(), "note": note } })
        }
        _ => json!({ "ended": { "reason": ended_text(why) } }),
    }
}

/// A lease's events as one JSON object per line, ending after a terminal event
///
/// Dropping it drops the engine's receiver, which is how a hang-up reaches
/// the engine.
#[derive(Debug)]
pub(crate) struct LeaseStream {
    events: Option<LeaseEvents>,
    model: Option<ModelName>,
    config: watch::Receiver<Arc<Config>>,
    clock: Clock,
    beat: Pin<Box<Sleep>>,
}

impl LeaseStream {
    pub(crate) fn new(
        events: LeaseEvents,
        model: Option<ModelName>,
        config: watch::Receiver<Arc<Config>>,
        clock: Clock,
    ) -> Self {
        Self {
            events: Some(events),
            model,
            config,
            clock,
            beat: Box::pin(sleep(STREAM_HEARTBEAT)),
        }
    }

    /// The line for `event`, and whether the stream ends after it
    ///
    /// A refusal carries `expected_until` in its body and no `Retry-After`, since the status
    /// line is already sent.
    fn render(&self, event: &LeaseEvent) -> (Value, bool) {
        let model = self.model.as_ref();
        match event {
            LeaseEvent::Waiting { reason, estimate } => (
                json!({ "queued": {
                    "reason": reply::sentence(reason, &self.clock),
                    "reason_kind": reply::kind(reason),
                    "estimate": estimate.map(|at| at.to_string()),
                } }),
                false,
            ),
            LeaseEvent::Granted { lease } => {
                let reconnect = self.config.borrow().reconnect;
                (
                    json!({ "granted": {
                        "id": render_id(*lease),
                        "reconnect": duration_text(reconnect),
                    } }),
                    false,
                )
            }
            LeaseEvent::Refused(refusal) => (
                json!({ "refused": reply::busy_body(model, refusal, &self.clock) }),
                true,
            ),
            LeaseEvent::Failed(error) => (
                json!({ "failed": {
                    "error": "failed",
                    "model": model.map(ModelName::as_str),
                    "reason": error,
                } }),
                true,
            ),
            LeaseEvent::Ended(why) => (ended_line(why), true),
        }
    }

    fn frame(&mut self, line: &Value) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.beat.as_mut().reset(Instant::now() + STREAM_HEARTBEAT);
        let mut bytes = serde_json::to_vec(line).unwrap_or_default();
        bytes.push(b'\n');
        Poll::Ready(Some(Ok(Frame::data(Bytes::from(bytes)))))
    }
}

impl HttpBody for LeaseStream {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = &mut *self;
        let Some(events) = this.events.as_mut() else {
            return Poll::Ready(None);
        };
        match events.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                let (line, last) = this.render(&event);
                if last {
                    // Dropped here: after a terminal event the engine has nothing more to say.
                    this.events = None;
                }
                this.frame(&line)
            }
            Poll::Ready(None) => {
                this.events = None;
                Poll::Ready(None)
            }
            Poll::Pending => match this.beat.as_mut().poll(cx) {
                Poll::Ready(()) => this.frame(&json!({ "heartbeat": {} })),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.events.is_none()
    }
}
