//! Reports the engine's queue to the shepherd's metrics dog, as gauges that keep their history.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use shep_channel::{MetricOptions, MetricType};
use shep_client::dogs::Stop;
use tokio::time::{MissedTickBehavior, interval};

use crate::{
    book::{Snapshot, State},
    config::ModelName,
    engine::EngineHandle,
    http::reply,
};

/// How often every series is sent again
pub(crate) const REPORT_EVERY: Duration = Duration::from_secs(15);

/// A series' name and help text
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Series {
    /// The metric name.
    pub name: &'static str,
    /// What it counts.
    pub help: &'static str,
}

const MODEL_LOADED: Series = Series {
    name: "paddock_model_loaded",
    help: "1 while the model is loaded, 0 in any other state.",
};
const LEASES: Series = Series {
    name: "paddock_leases",
    help: "Granted leases on the model. A bare lease has no model label.",
};
const WAITERS: Series = Series {
    name: "paddock_waiters",
    help: "Requests and leases waiting, by what they were last told they wait for.",
};

/// The reason a waiter that has not been told one is counted under
const NO_REASON: &str = "none";

type Labels = BTreeMap<&'static str, String>;
type Key = (Series, Labels);

/// One gauge sample
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sample {
    /// Which series it belongs to.
    pub series: Series,
    /// Its labels.
    pub labels: Labels,
    /// Its value.
    pub value: f64,
}

/// The label sets of the series sent last time
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Sent(BTreeSet<Key>);

/// The samples to send for `snapshot`, and what was sent
///
/// A label set in `sent` and absent from `snapshot` is sent once more at 0: the metrics dog keeps
/// a series' last value until the dog restarts, so that is the only way a count stops showing.
pub(crate) fn series(snapshot: &Snapshot, sent: &Sent) -> (Vec<Sample>, Sent) {
    let mut values: BTreeMap<Key, u32> = BTreeMap::new();
    for model in &snapshot.models {
        let labels = Labels::from([("model", model.name.to_string())]);
        values.insert(
            (MODEL_LOADED, labels),
            u32::from(model.state == State::Loaded),
        );
    }
    for lease in snapshot
        .leases
        .iter()
        .filter(|lease| lease.revoked.is_none())
    {
        let labels = model_labels(lease.model.as_ref());
        *values.entry((LEASES, labels)).or_default() += 1;
    }
    for waiter in &snapshot.waiters {
        let mut labels = model_labels(waiter.model.as_ref());
        labels.insert(
            "reason_kind",
            waiter
                .reason
                .as_ref()
                .map_or(NO_REASON, reply::kind)
                .to_owned(),
        );
        *values.entry((WAITERS, labels)).or_default() += 1;
    }
    let gone = sent.0.iter().filter(|key| !values.contains_key(*key));
    let mut samples: Vec<Sample> = gone
        .map(|(series, labels)| Sample {
            series: *series,
            labels: labels.clone(),
            value: 0.0,
        })
        .collect();
    let now = Sent(values.keys().cloned().collect());
    samples.extend(values.into_iter().map(|((series, labels), count)| Sample {
        series,
        labels,
        value: f64::from(count),
    }));
    (samples, now)
}

fn model_labels(model: Option<&ModelName>) -> Labels {
    model
        .map(|model| ("model", model.to_string()))
        .into_iter()
        .collect()
}

/// Where samples go: the shepherd's channel, or a recording fake
pub(crate) trait Channel {
    /// Whether a sample sent now reaches the shepherd.
    fn is_active(&self) -> bool;
    /// Sends one gauge sample.
    fn send(&self, sample: &Sample);
}

impl Channel for shep_channel::Shepherd {
    fn is_active(&self) -> bool {
        Self::is_active(self)
    }

    fn send(&self, sample: &Sample) {
        let options = sample.labels.iter().fold(
            MetricOptions::new()
                .kind(MetricType::Gauge)
                .help(sample.series.help),
            |options, (key, value)| options.label(*key, value.as_str()),
        );
        self.metric_with(sample.series.name, sample.value, options);
    }
}

/// Sends the engine's series every [`REPORT_EVERY`] until a stop is requested
///
/// Returns at once for a dog with no channel, without asking the engine for anything.
pub(crate) async fn report<C: Channel>(channel: &C, engine: &EngineHandle, mut stop: Stop) {
    if !channel.is_active() {
        return;
    }
    let mut every = interval(REPORT_EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sent = Sent::default();
    loop {
        tokio::select! {
            biased;
            () = stop.wait() => return,
            _ = every.tick() => {}
        }
        let snapshot = tokio::select! {
            biased;
            () = stop.wait() => return,
            snapshot = engine.snapshot() => snapshot,
        };
        let (samples, now) = series(&snapshot, &sent);
        for sample in &samples {
            channel.send(sample);
        }
        sent = now;
    }
}

#[cfg(test)]
mod tests;
