//! A loaded Strata model's own numbers, read from its `/metrics` and reported by model.
//!
//! A scrape of the sheep itself would lose its figures at every model swap, and its `/metrics` is
//! JSON, so the dog reads it for the model that is loaded and sends the figures on.

use core::fmt;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    time::Duration,
};

use futures_util::future::join_all;
use serde::{Deserialize, Deserializer};
use tokio::time::timeout;

use super::{Key, Kind, Labels, Sample, Series};
use crate::{
    book::{Snapshot, State},
    config::{Config, MetricsKind, Model, ModelName},
};

/// How long a scrape may take, so a stuck backend never delays the dog's own series
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);

/// The words `live.state` takes, each reported as its own series
const STATES: [&str; 4] = ["reading", "generating", "idle", "unloaded"];

const fn counter(name: &'static str, help: &'static str) -> Series {
    Series {
        name,
        help,
        kind: Kind::Counter,
    }
}

const fn gauge(name: &'static str, help: &'static str) -> Series {
    Series {
        name,
        help,
        kind: Kind::Gauge,
    }
}

const REQUESTS: Series = counter(
    "paddock_strata_requests_total",
    "Requests the Strata backend has served since it started.",
);
const PROMPT_TOKENS: Series = counter(
    "paddock_strata_prompt_tokens_total",
    "Prompt tokens the Strata backend has read since it started.",
);
const OUTPUT_TOKENS: Series = counter(
    "paddock_strata_output_tokens_total",
    "Tokens the Strata backend has generated since it started.",
);
const REUSED_TOKENS: Series = counter(
    "paddock_strata_reused_tokens_total",
    "Prompt tokens the Strata backend reused from a warm conversation since it started.",
);
const DRAFTS_OFFERED: Series = counter(
    "paddock_strata_drafts_offered_total",
    "Draft tokens the Strata backend offered for checking since it started.",
);
const DRAFTS_ACCEPTED: Series = counter(
    "paddock_strata_drafts_accepted_total",
    "Draft tokens the Strata backend accepted since it started.",
);
const PROMPT_SECONDS: Series = counter(
    "paddock_strata_prompt_seconds_total",
    "Seconds the Strata backend has spent reading prompts since it started.",
);
const DECODE_SECONDS: Series = counter(
    "paddock_strata_decode_seconds_total",
    "Seconds the Strata backend has spent generating since it started.",
);
const QUEUED: Series = gauge(
    "paddock_strata_queued",
    "Requests queued inside the Strata backend.",
);
const TOKENS_PER_SECOND: Series = gauge(
    "paddock_strata_tokens_per_second",
    "Tokens per second the Strata backend is generating, 0 while it is not.",
);
const STATE: Series = gauge(
    "paddock_strata_state",
    "1 for the state the Strata backend is in, 0 for the others.",
);
const GPU_UTILIZATION: Series = gauge(
    "paddock_strata_gpu_utilization_percent",
    "GPU utilization as the Strata backend reads it.",
);
const GPU_MEMORY_USED: Series = gauge(
    "paddock_strata_gpu_memory_used_bytes",
    "GPU memory in use as the Strata backend reads it.",
);
const GPU_TEMPERATURE: Series = gauge(
    "paddock_strata_gpu_temperature_celsius",
    "GPU temperature as the Strata backend reads it.",
);
const GPU_POWER: Series = gauge(
    "paddock_strata_gpu_power_watts",
    "GPU power draw as the Strata backend reads it.",
);

/// Strata's `/metrics`, read leniently: every field may be missing or `null`, and a field the dog
/// does not know is skipped
#[derive(Debug, Default, Deserialize)]
struct Reply {
    totals: Option<Totals>,
    live: Option<Live>,
    hardware: Option<Hardware>,
}

#[derive(Debug, Default, Deserialize)]
struct Totals {
    requests: Option<f64>,
    prompt_tokens: Option<f64>,
    reused: Option<f64>,
    output_tokens: Option<f64>,
    drafts_offered: Option<f64>,
    drafts_accepted: Option<f64>,
    prompt_ms: Option<f64>,
    decode_ms: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct Live {
    queued: Option<f64>,
    /// `Some(None)` is a `null`, which Strata sends while it is not generating.
    #[serde(default, deserialize_with = "present")]
    tok_s: Option<Option<f64>>,
    state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Hardware {
    gpu_util: Option<f64>,
    gpu_mem_used: Option<f64>,
    gpu_temp: Option<f64>,
    gpu_power: Option<f64>,
}

/// A field that was sent, `null` or not, as opposed to one that was left out
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<f64>>, D::Error> {
    Option::<f64>::deserialize(deserializer).map(Some)
}

/// The samples for a reply from `model`'s backend
///
/// A field that is missing or `null` has no sample, except `tok_s`, which reads 0 when it is
/// `null`.
///
/// # Errors
/// If `body` is not a JSON object whose known fields are numbers, or a word for `state`.
fn samples(model: &ModelName, body: &[u8]) -> Result<Vec<Sample>, serde_json::Error> {
    let reply: Reply = serde_json::from_slice(body)?;
    let totals = reply.totals.unwrap_or_default();
    let live = reply.live.unwrap_or_default();
    let hardware = reply.hardware.unwrap_or_default();
    let seconds = |ms: Option<f64>| ms.map(|ms| ms / 1000.0);

    let readings = [
        (REQUESTS, totals.requests),
        (PROMPT_TOKENS, totals.prompt_tokens),
        (REUSED_TOKENS, totals.reused),
        (OUTPUT_TOKENS, totals.output_tokens),
        (DRAFTS_OFFERED, totals.drafts_offered),
        (DRAFTS_ACCEPTED, totals.drafts_accepted),
        (PROMPT_SECONDS, seconds(totals.prompt_ms)),
        (DECODE_SECONDS, seconds(totals.decode_ms)),
        (QUEUED, live.queued),
        (
            TOKENS_PER_SECOND,
            live.tok_s.map(|tok_s| tok_s.unwrap_or(0.0)),
        ),
        (GPU_UTILIZATION, hardware.gpu_util),
        (GPU_MEMORY_USED, hardware.gpu_mem_used),
        (GPU_TEMPERATURE, hardware.gpu_temp),
        (GPU_POWER, hardware.gpu_power),
    ];
    let by_model = Labels::from([("model", model.to_string())]);
    let mut samples: Vec<Sample> = readings
        .into_iter()
        .filter_map(|(series, value)| {
            Some(Sample {
                series,
                labels: by_model.clone(),
                value: value?,
            })
        })
        .collect();
    if let Some(current) = live.state {
        samples.extend(STATES.iter().map(|state| {
            let mut labels = by_model.clone();
            labels.insert("state", (*state).to_owned());
            Sample {
                series: STATE,
                labels,
                value: f64::from(u8::from(*state == current)),
            }
        }));
    }
    Ok(samples)
}

/// Why a scrape gave nothing, in words that name no url: a url may carry a credential
#[derive(Debug)]
enum Failure {
    TimedOut,
    Unreachable,
    Status(u16),
    NotMetrics,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => write!(f, "it did not answer within {}s", SCRAPE_TIMEOUT.as_secs()),
            Self::Unreachable => f.write_str("the request failed"),
            Self::Status(status) => write!(f, "it answered {status}"),
            Self::NotMetrics => f.write_str("it did not answer the JSON Strata gives"),
        }
    }
}

/// Reads each loaded Strata model's `/metrics`, and remembers what it has sent
#[derive(Debug)]
pub(super) struct Scraper {
    client: reqwest::Client,
    /// The gauge label sets last sent for each model, which get a zero when a scrape leaves one out
    /// or the model stops being scraped.
    gauged: BTreeMap<ModelName, BTreeSet<Key>>,
    /// The models whose failure is logged, until a scrape of them succeeds or they stop being
    /// scraped.
    failing: BTreeSet<ModelName>,
}

impl Scraper {
    pub(super) fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            gauged: BTreeMap::new(),
            failing: BTreeSet::new(),
        }
    }

    /// The Strata samples to send for `snapshot`
    ///
    /// Each model that is loaded and marked `metrics = "strata"` in `config` is read, all at once.
    /// One whose scrape fails sends nothing, and `log` is told once. A model that was read and no
    /// longer is gets its gauges sent once at 0, and its counters left as they were, since a
    /// counter that falls reads as a reset. A gauge that a later scrape of a model leaves out is
    /// sent once at 0 in the same way.
    pub(super) async fn samples(
        &mut self,
        snapshot: &Snapshot,
        config: &Config,
        log: &mut impl Write,
    ) -> Vec<Sample> {
        let targets: Vec<(&Model, &str)> = snapshot
            .models
            .iter()
            .filter(|view| view.state == State::Loaded)
            .filter_map(|view| config.models.get(&view.name))
            .filter(|model| model.metrics == Some(MetricsKind::Strata))
            .filter_map(|model| Some((model, model.url.as_deref()?)))
            .collect();
        let replies = join_all(targets.iter().map(|(model, url)| self.scrape(model, url))).await;
        self.failing
            .retain(|name| targets.iter().any(|(model, _)| model.name == *name));

        let mut samples = Vec::new();
        for ((model, _), reply) in targets.iter().zip(replies) {
            match reply {
                Ok(read) => {
                    self.failing.remove(&model.name);
                    let now: BTreeSet<Key> = read
                        .iter()
                        .filter(|sample| sample.series.kind == Kind::Gauge)
                        .map(|sample| (sample.series, sample.labels.clone()))
                        .collect();
                    let kept = self.gauged.entry(model.name.clone()).or_default();
                    for (series, labels) in kept.difference(&now) {
                        samples.push(Sample {
                            series: *series,
                            labels: labels.clone(),
                            value: 0.0,
                        });
                    }
                    *kept = now;
                    samples.extend(read);
                }
                Err(failure) => {
                    if self.failing.insert(model.name.clone()) {
                        let name = &model.name;
                        let _ = writeln!(
                            log,
                            "paddock: reading {name}'s Strata metrics failed, and later failures are not logged until one succeeds: {failure}"
                        );
                    }
                }
            }
        }
        let scraped: BTreeSet<&ModelName> = targets.iter().map(|(model, _)| &model.name).collect();
        let stopped: Vec<ModelName> = self
            .gauged
            .keys()
            .filter(|name| !scraped.contains(name))
            .cloned()
            .collect();
        for name in stopped {
            for (series, labels) in self.gauged.remove(&name).unwrap_or_default() {
                samples.push(Sample {
                    series,
                    labels,
                    value: 0.0,
                });
            }
        }
        samples
    }

    async fn scrape(&self, model: &Model, url: &str) -> Result<Vec<Sample>, Failure> {
        let mut request = self.client.get(format!("{url}/metrics"));
        if let Some(key) = model.key() {
            request = request.bearer_auth(key);
        }
        let read = async {
            let response = request.send().await.map_err(|_| Failure::Unreachable)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(Failure::Status(response.status().as_u16()));
            }
            let body = response.bytes().await.map_err(|_| Failure::Unreachable)?;
            samples(&model.name, &body).map_err(|_| Failure::NotMetrics)
        };
        timeout(SCRAPE_TIMEOUT, read)
            .await
            .unwrap_or(Err(Failure::TimedOut))
    }
}

#[cfg(test)]
mod tests;
