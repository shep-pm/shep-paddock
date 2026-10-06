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
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, Sleep, sleep},
};

use super::{duration_text, render_id};
use crate::{
    book::Ended,
    config::{Config, ModelName},
    engine::{Clock, LeaseEvent},
    http::reply,
};

/// How often a stream says it is alive, so a dead peer is noticed
pub(crate) const STREAM_HEARTBEAT: Duration = Duration::from_secs(15);

/// How a lease ended, as the stream says it
pub(crate) fn ended_text(why: Ended) -> &'static str {
    match why {
        Ended::Released => "released",
        Ended::Expired => "expired",
        Ended::Abandoned => "abandoned",
    }
}

/// A lease's events as one JSON object per line, ending after a terminal event
///
/// Dropping it drops the engine's receiver, which is how a hang-up reaches
/// the engine.
#[derive(Debug)]
pub(crate) struct LeaseStream {
    events: Option<mpsc::Receiver<LeaseEvent>>,
    model: Option<ModelName>,
    config: watch::Receiver<Arc<Config>>,
    clock: Clock,
    beat: Pin<Box<Sleep>>,
}

impl LeaseStream {
    pub(crate) fn new(
        events: mpsc::Receiver<LeaseEvent>,
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
    fn render(&self, event: &LeaseEvent) -> (Value, bool) {
        // An attached stream never carries a refusal or a failure, so it need not know its model.
        let model = self.model.clone().unwrap_or_else(|| ModelName::from(""));
        match event {
            LeaseEvent::Waiting { reason, estimate } => (
                json!({ "queued": {
                    "reason": reply::sentence(reason, &self.clock),
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
                json!({ "refused": reply::busy_body(&model, refusal, &self.clock) }),
                true,
            ),
            LeaseEvent::Failed(error) => (
                json!({ "failed": {
                    "error": "failed",
                    "model": model.as_str(),
                    "reason": error,
                } }),
                true,
            ),
            LeaseEvent::Ended(why) => (json!({ "ended": { "why": ended_text(*why) } }), true),
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
