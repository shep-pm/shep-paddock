//! What the host pays for electricity, from the config or looked up from where the host is.
//!
//! A price in `[meter]` is the maintainer's bill and wins. Without one, a free EIA API key named
//! by `eia_key_env` turns on a lookup: ipinfo.io places the host's address, and for a US host EIA
//! gives its state's average residential price in the latest month it has. That average stands in
//! for the host's own tariff, whose tiers and time-of-use rates do not reduce to one figure, and
//! it runs two to three months behind. Outside the US the host is unpriced.

use core::{fmt, time::Duration};
use std::{collections::BTreeSet, io::Write};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use reqwest::Url;
use serde_json::Value;
use shep_client::dogs::Stop;
use tokio::{sync::watch, time::sleep};

use crate::config::MeterConfig;

mod states;

/// Where the host's address is placed
const IPINFO: &str = "https://ipinfo.io/json";
/// EIA's retail sales, one row per state, sector and month
const EIA: &str = "https://api.eia.gov/v2/electricity/retail-sales/data";

/// How long a looked-up price is kept before it is read again
///
/// EIA publishes monthly, so a week is the longest wait that still catches a new month within days.
const REFRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long a failed lookup waits before trying again
const RETRY: Duration = Duration::from_secs(60 * 60);
/// How long one request has to be answered
const LIMIT: Duration = Duration::from_secs(10);

/// Where a price came from
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The maintainer's own figure, `usd_per_kwh` in `[meter]`.
    Set,
    /// EIA's state average for the latest month it has.
    Eia {
        /// The state's full name, as ipinfo gives it.
        region: String,
        /// The month the average is for, as `YYYY-MM`.
        period: String,
    },
}

/// The host's electricity price
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Price {
    /// US dollars per kWh.
    pub usd_per_kwh: f64,
    /// Where it came from.
    pub origin: Origin,
}

/// Why a request got no answer
///
/// Carries no url: a request's url holds the API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FetchError {
    /// The connection was refused or dropped, or the name did not resolve.
    Unreachable,
    /// No answer within the limit.
    TimedOut,
    /// The service answered with this status instead of 200.
    Refused(u16),
    /// The answer is not what was asked for.
    Unreadable,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => f.write_str("the service refused or dropped the connection"),
            Self::TimedOut => f.write_str("the service did not answer in time"),
            Self::Refused(status) => write!(f, "the service answered {status}"),
            Self::Unreadable => f.write_str("the service's answer is not what was asked for"),
        }
    }
}

impl core::error::Error for FetchError {}

/// Why a lookup gave no price
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LookupError {
    /// The variable `eia_key_env` names is unset or empty.
    NoKey,
    /// ipinfo places the host outside the US, where EIA has no price.
    OutsideUs,
    /// ipinfo's region is no US state or district, such as Puerto Rico.
    UnknownRegion,
    /// ipinfo.io gave no usable answer.
    Locating(FetchError),
    /// api.eia.gov gave no usable answer.
    Pricing(FetchError),
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKey => f.write_str("the key variable is unset or empty"),
            Self::OutsideUs => f.write_str("the host is outside the US"),
            Self::UnknownRegion => f.write_str("the host's region is no US state"),
            Self::Locating(why) => write!(f, "ipinfo.io: {why}"),
            Self::Pricing(why) => write!(f, "api.eia.gov: {why}"),
        }
    }
}

impl core::error::Error for LookupError {}

/// How a url is fetched
///
/// Boxed futures so it can sit behind `dyn`.
pub(crate) trait Fetch: fmt::Debug {
    /// The body of the `200` answer to `GET url`.
    fn get(&self, url: &Url) -> LocalBoxFuture<'_, Result<String, FetchError>>;
}

/// The dog's HTTP client, as [`Fetch`]
#[derive(Debug)]
pub(crate) struct Web {
    client: reqwest::Client,
}

impl Web {
    /// A web that sends through `client`
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Fetch for Web {
    fn get(&self, url: &Url) -> LocalBoxFuture<'_, Result<String, FetchError>> {
        let request = self.client.get(url.clone()).timeout(LIMIT);
        async move {
            let response = request.send().await.map_err(classify)?;
            let status = response.status();
            if status != reqwest::StatusCode::OK {
                return Err(FetchError::Refused(status.as_u16()));
            }
            response.text().await.map_err(classify)
        }
        .boxed_local()
    }
}

/// What went wrong, without the url reqwest's own message carries
fn classify(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        FetchError::TimedOut
    } else if err.is_decode() || err.is_body() {
        FetchError::Unreadable
    } else {
        FetchError::Unreachable
    }
}

/// Keeps `prices` at the host's price until a stop is requested
///
/// A `usd_per_kwh` in `settings` is the price and nothing is looked up. With only `eia_key_env`,
/// the price is looked up through `web` with the key `env` gives for that variable, read at each
/// lookup, and again after a week. A lookup that fails keeps the last price, is tried again within
/// the hour, and has its reason logged to `log` once until one succeeds. With neither key, `prices`
/// stays `None` and this returns at once.
pub(crate) async fn keep(
    settings: &MeterConfig,
    web: &dyn Fetch,
    env: impl Fn(&str) -> Option<String>,
    prices: &watch::Sender<Option<Price>>,
    mut stop: Stop,
    mut log: impl Write,
) {
    if let Some(price) = settings.usd_per_kwh {
        prices.send_replace(Some(Price {
            usd_per_kwh: price.get(),
            origin: Origin::Set,
        }));
        return;
    }
    let Some(variable) = settings.eia_key_env.as_deref() else {
        return;
    };
    let mut logged = BTreeSet::new();
    loop {
        let key = env(variable).filter(|key| !key.is_empty());
        let looked_up = tokio::select! {
            biased;
            () = stop.wait() => return,
            looked_up = look_up(web, key.as_deref()) => looked_up,
        };
        let wait = match looked_up {
            Ok(price) => {
                logged.clear();
                prices.send_replace(Some(price));
                REFRESH
            }
            Err(reason) => {
                if logged.insert(reason) {
                    let _ = writeln!(
                        log,
                        "paddock: the host's electricity price goes unread: {reason}"
                    );
                }
                RETRY
            }
        };
        tokio::select! {
            biased;
            () = stop.wait() => return,
            () = sleep(wait) => {}
        }
    }
}

/// The price EIA gives for the state ipinfo places the host in
async fn look_up(web: &dyn Fetch, key: Option<&str>) -> Result<Price, LookupError> {
    let key = key.ok_or(LookupError::NoKey)?;
    let located = web
        .get(&Url::parse(IPINFO).map_err(|_| LookupError::Locating(FetchError::Unreadable))?)
        .await
        .map_err(LookupError::Locating)?;
    let region = region_in(&located)?;
    let code = states::code(&region).ok_or(LookupError::UnknownRegion)?;
    let priced = web
        .get(&eia_url(key, code)?)
        .await
        .map_err(LookupError::Pricing)?;
    let (cents, period) =
        latest_price_in(&priced).ok_or(LookupError::Pricing(FetchError::Unreadable))?;
    Ok(Price {
        usd_per_kwh: cents / 100.0,
        origin: Origin::Eia { region, period },
    })
}

/// The latest residential price in `state`, one row, newest month first
fn eia_url(key: &str, state: &str) -> Result<Url, LookupError> {
    Url::parse_with_params(
        EIA,
        [
            ("api_key", key),
            ("frequency", "monthly"),
            ("data[]", "price"),
            ("facets[sectorid][]", "RES"),
            ("facets[stateid][]", state),
            ("sort[0][column]", "period"),
            ("sort[0][direction]", "desc"),
            ("length", "1"),
        ],
    )
    .map_err(|_| LookupError::Pricing(FetchError::Unreadable))
}

/// The state's name in an ipinfo answer, if the host is in the US
fn region_in(answer: &str) -> Result<String, LookupError> {
    let unreadable = LookupError::Locating(FetchError::Unreadable);
    let answer: Value = serde_json::from_str(answer).map_err(|_| unreadable)?;
    let text = |field: &str| answer.get(field).and_then(Value::as_str);
    if text("country").ok_or(unreadable)? != "US" {
        return Err(LookupError::OutsideUs);
    }
    text("region").map(str::to_owned).ok_or(unreadable)
}

/// The price in cents per kWh and the month it is for, from an EIA answer's first row
///
/// EIA writes numbers as strings, and a number is read as well.
fn latest_price_in(answer: &str) -> Option<(f64, String)> {
    let answer: Value = serde_json::from_str(answer).ok()?;
    let row = answer.pointer("/response/data/0")?;
    let cents = match row.get("price")? {
        Value::String(text) => text.parse().ok()?,
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    let period = row.get("period")?.as_str()?.to_owned();
    (f64::is_finite(cents) && cents >= 0.0).then_some((cents, period))
}

#[cfg(test)]
mod tests;
