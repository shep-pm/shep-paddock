//! Reports the engine's queue to the shepherd's metrics dog, as gauges that keep their history,
//! and a loaded Strata model's own numbers beside them.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::Arc,
    time::Duration,
};

use shep_channel::{MetricOptions, MetricType};
use shep_client::dogs::Stop;
use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};

use crate::{
    book::{Snapshot, State},
    config::{Config, ModelName},
    engine::{EngineHandle, Share, Use},
    http::reply,
    meter::Reading,
    outbound::http_client,
    price::Price,
};

mod strata;

/// How often every series is sent again
pub(crate) const REPORT_EVERY: Duration = Duration::from_secs(15);

/// How the metrics dog treats a series' values
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// A value that goes up and down.
    Gauge,
    /// A total that only grows, until its source restarts.
    Counter,
}

/// A series' name and help text
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Series {
    /// The metric name.
    pub name: &'static str,
    /// What it counts.
    pub help: &'static str,
    /// Whether it is a gauge or a counter.
    pub kind: Kind,
}

const MODEL_LOADED: Series = Series {
    name: "paddock_model_loaded",
    help: "1 while the model is loaded, 0 in any other state.",
    kind: Kind::Gauge,
};
const LEASES: Series = Series {
    name: "paddock_leases",
    help: "Granted leases on the model. A bare lease has no model label.",
    kind: Kind::Gauge,
};
const WAITERS: Series = Series {
    name: "paddock_waiters",
    help: "Requests and leases waiting, by what they were last told they wait for.",
    kind: Kind::Gauge,
};

const ELECTRICITY_PRICE: Series = Series {
    name: "paddock_electricity_usd_per_kwh",
    help: "What the host pays for electricity, in US dollars per kWh. Absent while it has no price.",
    kind: Kind::Gauge,
};
const POWER_WATTS: Series = Series {
    name: "paddock_power_watts",
    help: "The host's power at the last read, by what measured it: wall for a plug, parts for the GPU and CPU package alone.",
    kind: Kind::Gauge,
};
const ENERGY_JOULES: Series = Series {
    name: "paddock_energy_joules_total",
    help: "Energy the host used since the dog started, by what measured it and whether a lease with a request in flight used it or none did.",
    kind: Kind::Counter,
};

/// The reason a waiter that has not been told one is counted under
const NO_REASON: &str = "none";

type Labels = BTreeMap<&'static str, String>;
type Key = (Series, Labels);

/// One sample
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

/// The energy the meter has added up, by the model whose leases used it and what it was used for
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Totals(BTreeMap<(Option<ModelName>, Use), f64>);

impl Totals {
    /// Adds `shares` to the totals
    pub(crate) fn add(&mut self, shares: &[Share]) {
        for share in shares {
            *self.0.entry((share.model.clone(), share.used)).or_default() += share.joules;
        }
    }
}

/// The power `reading` shows and the energy `totals` add up to, for the metrics dog
///
/// Each carries the `meter` label, `wall` or `parts`, and the energy a `use` label and, for a
/// lease's, its `model`. The metrics dog writes `source` on every sample itself and refuses a
/// sample that sets it, so which meter read the power cannot be labelled that.
pub(crate) fn power(reading: &Reading, totals: &Totals) -> Vec<Sample> {
    let meter = || Labels::from([("meter", reading.source.label().to_owned())]);
    let mut samples = vec![Sample {
        series: POWER_WATTS,
        labels: meter(),
        value: reading.watts,
    }];
    for ((model, used), joules) in &totals.0 {
        let mut labels = meter();
        labels.insert("use", used.label().to_owned());
        if let Some(model) = model {
            labels.insert("model", model.to_string());
        }
        samples.push(Sample {
            series: ENERGY_JOULES,
            labels,
            value: *joules,
        });
    }
    samples
}

/// The price `price` shows, for the metrics dog
pub(crate) fn electricity(price: &Price) -> Sample {
    Sample {
        series: ELECTRICITY_PRICE,
        labels: Labels::new(),
        value: price.usd_per_kwh,
    }
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
    /// Sends one sample, as the kind its series says.
    fn send(&self, sample: &Sample);
}

impl Channel for shep_channel::Shepherd {
    fn is_active(&self) -> bool {
        Self::is_active(self)
    }

    fn send(&self, sample: &Sample) {
        let options = sample.labels.iter().fold(
            MetricOptions::new()
                .kind(match sample.series.kind {
                    Kind::Gauge => MetricType::Gauge,
                    Kind::Counter => MetricType::Counter,
                })
                .help(sample.series.help),
            |options, (key, value)| options.label(*key, value.as_str()),
        );
        self.metric_with(sample.series.name, sample.value, options);
    }
}

/// Sends the engine's series every [`REPORT_EVERY`] until a stop is requested
///
/// A loaded model that `config` marks `metrics = "strata"` has its backend read after the
/// engine's series are sent, so a backend that does not answer holds up nothing of the dog's own.
///
/// The host's price is sent with them once it is known.
///
/// Returns at once for a dog with no channel, without asking the engine for anything.
pub(crate) async fn report<C: Channel>(
    channel: &C,
    engine: &EngineHandle,
    config: watch::Receiver<Arc<Config>>,
    price: watch::Receiver<Option<Price>>,
    stop: Stop,
) {
    report_logging(channel, engine, config, price, stop, std::io::stderr()).await;
}

/// [`report`], with the failed scrapes logged to `log`
async fn report_logging<C: Channel>(
    channel: &C,
    engine: &EngineHandle,
    config: watch::Receiver<Arc<Config>>,
    price: watch::Receiver<Option<Price>>,
    mut stop: Stop,
    mut log: impl Write,
) {
    if !channel.is_active() {
        return;
    }
    let mut every = interval(REPORT_EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sent = Sent::default();
    let mut strata = strata::Scraper::new(http_client());
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
        let known = price.borrow().as_ref().map(electricity);
        if let Some(sample) = known {
            channel.send(&sample);
        }
        let config = Arc::clone(&config.borrow());
        let read = tokio::select! {
            biased;
            () = stop.wait() => return,
            read = strata.samples(&snapshot, &config, &mut log) => read,
        };
        for sample in &read {
            channel.send(sample);
        }
    }
}

#[cfg(test)]
mod tests;
