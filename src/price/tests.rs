//! The price from the config or from the lookup, kept on a paused clock beside a scripted web.

use core::time::Duration;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
    rc::Rc,
};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use reqwest::Url;
use shep_client::dogs::Stop;
use tokio::{sync::watch, time::sleep};

use super::{Fetch, FetchError, Origin, Price, Web, keep};
use crate::config::Config;
use crate::http::status::price_body;
use crate::test_support::HOST_AND_MODELS;

const IPINFO: &str = include_str!("testdata/ipinfo.json");
const EIA: &str = include_str!("testdata/eia-retail-sales.json");

const KEY: &str = "eia-key-0123456789abcdef";
const KEY_VARIABLE: &str = "EIA_API_KEY";

const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

type Script = BTreeMap<&'static str, VecDeque<Result<String, FetchError>>>;

/// A web that answers each host from its own script, in order, the last answer repeating, and
/// keeps every url it was asked. Clones share the script and the record.
#[derive(Debug, Clone, Default)]
struct FakeWeb {
    scripts: Rc<RefCell<Script>>,
    asked: Rc<RefCell<Vec<Url>>>,
}

impl FakeWeb {
    fn answering(host: &'static str, answers: Vec<Result<&str, FetchError>>) -> Self {
        Self::default().and(host, answers)
    }

    fn and(self, host: &'static str, answers: Vec<Result<&str, FetchError>>) -> Self {
        let answers = answers
            .into_iter()
            .map(|answer| answer.map(str::to_owned))
            .collect();
        self.scripts.borrow_mut().insert(host, answers);
        self
    }

    /// Texas, as ipinfo and EIA answer for it
    fn texas() -> Self {
        Self::answering("ipinfo.io", vec![Ok(IPINFO)]).and("api.eia.gov", vec![Ok(EIA)])
    }

    fn asked(&self) -> Vec<Url> {
        self.asked.borrow().clone()
    }

    fn asked_of(&self, host: &str) -> usize {
        self.asked()
            .iter()
            .filter(|url| url.host_str() == Some(host))
            .count()
    }
}

impl Fetch for FakeWeb {
    fn get(&self, url: &Url) -> LocalBoxFuture<'_, Result<String, FetchError>> {
        self.asked.borrow_mut().push(url.clone());
        let host = url.host_str().unwrap_or_default();
        let answer = self
            .scripts
            .borrow_mut()
            .get_mut(host)
            .and_then(|script| {
                if script.len() > 1 {
                    script.pop_front()
                } else {
                    script.front().cloned()
                }
            })
            .unwrap_or(Err(FetchError::Unreachable));
        async move { answer }.boxed_local()
    }
}

/// A log the test reads while the price keeper still holds a writer to it
#[derive(Debug, Clone, Default)]
struct Log(Rc<RefCell<Vec<u8>>>);

impl Log {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).expect("utf8")
    }
}

impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `[meter]` table with `table` in it, as the dog reads it
fn settings(table: &str) -> crate::config::MeterConfig {
    Config::from_toml(&format!("{HOST_AND_MODELS}\n[meter]\n{table}"))
        .expect("a config")
        .meter
        .expect("a meter")
}

fn with_key(name: &str) -> impl Fn(&str) -> Option<String> {
    let name = name.to_owned();
    move |asked| (asked == name).then(|| KEY.to_owned())
}

fn no_environment(_: &str) -> Option<String> {
    None
}

/// Keeps the price from `table` and `web` while `body` runs, then returns what `body` returns
async fn keeping<T>(
    table: &str,
    web: &FakeWeb,
    env: impl Fn(&str) -> Option<String>,
    log: &Log,
    body: impl FnOnce(watch::Receiver<Option<Price>>) -> LocalBoxFuture<'static, T>,
) -> T {
    let (prices, seen) = watch::channel(None);
    let (stop, _request) = Stop::new();
    let settings = settings(table);
    let kept = keep(&settings, web, env, &prices, stop, log.clone());
    tokio::pin!(kept);
    let mut body = body(seen);
    // A keeper with nothing to look up returns at once, and the body still has its say.
    tokio::select! {
        () = &mut kept => body.await,
        done = &mut body => done,
    }
}

fn eia(usd_per_kwh: f64, region: &str, period: &str) -> Price {
    Price {
        usd_per_kwh,
        origin: Origin::Eia {
            region: region.to_owned(),
            period: period.to_owned(),
        },
    }
}

#[tokio::test(start_paused = true)]
async fn a_set_price_wins_over_a_lookup() {
    let web = FakeWeb::texas();
    let log = Log::default();
    let table = format!("usd_per_kwh = 0.17\neia_key_env = \"{KEY_VARIABLE}\"");
    let (stop, _request) = Stop::new();
    let (prices, seen) = watch::channel(None);

    keep(
        &settings(&table),
        &web,
        with_key(KEY_VARIABLE),
        &prices,
        stop,
        log.clone(),
    )
    .await;

    assert_eq!(
        *seen.borrow(),
        Some(Price {
            usd_per_kwh: 0.17,
            origin: Origin::Set
        })
    );
    assert_eq!(web.asked(), Vec::<Url>::new(), "a lookup was made");
}

#[tokio::test(start_paused = true)]
async fn ipinfos_region_and_eias_price_make_dollars_per_kwh() {
    let web = FakeWeb::texas();
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");

    let price = keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    // 15.2 cents per kWh, as the fixture has it.
    assert_eq!(price, Some(eia(0.152, "Texas", "2026-08")));
    let asked = web.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].as_str(), "https://ipinfo.io/json");
    assert_eq!(asked[1].path(), "/v2/electricity/retail-sales/data");
    assert_eq!(asked[1].scheme(), "https");
    let query: Vec<(String, String)> = asked[1]
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
    assert_eq!(
        query,
        [
            pair("api_key", KEY),
            pair("frequency", "monthly"),
            pair("data[]", "price"),
            pair("facets[sectorid][]", "RES"),
            pair("facets[stateid][]", "TX"),
            pair("sort[0][column]", "period"),
            pair("sort[0][direction]", "desc"),
            pair("length", "1"),
        ]
    );
    assert_eq!(log.text(), "");
}

#[tokio::test(start_paused = true)]
async fn a_host_outside_the_us_is_unpriced() {
    let germany = IPINFO
        .replace("\"US\"", "\"DE\"")
        .replace("Texas", "Bavaria");
    let web = FakeWeb::answering("ipinfo.io", vec![Ok(&germany)]).and("api.eia.gov", vec![Ok(EIA)]);
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");

    let price = keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    assert_eq!(price, None);
    assert_eq!(
        web.asked_of("api.eia.gov"),
        0,
        "EIA was asked about Bavaria"
    );
    assert_eq!(
        log.text(),
        "paddock: the host's electricity price goes unread: the host is outside the US\n"
    );
}

#[tokio::test(start_paused = true)]
async fn a_region_that_is_no_state_is_unpriced() {
    let island = IPINFO.replace("Texas", "Puerto Rico");
    let web = FakeWeb::answering("ipinfo.io", vec![Ok(&island)]).and("api.eia.gov", vec![Ok(EIA)]);
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");

    let price = keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    assert_eq!(price, None);
    assert_eq!(web.asked_of("api.eia.gov"), 0);
    assert_eq!(log.text().lines().count(), 1, "{}", log.text());
}

#[tokio::test(start_paused = true)]
async fn a_failed_lookup_keeps_the_last_price() {
    let newer = EIA
        .replace("\"15.2\"", "\"16.0\"")
        .replace("2026-08", "2026-09");
    let web = FakeWeb::answering("ipinfo.io", vec![Ok(IPINFO)]).and(
        "api.eia.gov",
        vec![
            Ok(EIA),
            Err(FetchError::TimedOut),
            Err(FetchError::TimedOut),
            Ok(&newer),
        ],
    );
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");
    let old = Some(eia(0.152, "Texas", "2026-08"));

    keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        let (web, log, old) = (web.clone(), log.clone(), old.clone());
        async move {
            sleep(Duration::from_secs(1)).await;
            assert_eq!(*seen.borrow(), old);
            assert_eq!(web.asked_of("api.eia.gov"), 1);

            sleep(WEEK - Duration::from_secs(10)).await;
            assert_eq!(web.asked_of("api.eia.gov"), 1, "read again before a week");

            // The week is up and the lookup fails: the price stays, and the log has one line.
            sleep(Duration::from_secs(30 * 60)).await;
            assert_eq!(web.asked_of("api.eia.gov"), 2);
            assert_eq!(*seen.borrow(), old);
            assert_eq!(log.text().lines().count(), 1, "{}", log.text());

            // The same failure again is not logged again.
            sleep(Duration::from_secs(60 * 60)).await;
            assert_eq!(web.asked_of("api.eia.gov"), 3);
            assert_eq!(*seen.borrow(), old);
            assert_eq!(log.text().lines().count(), 1, "{}", log.text());

            // The next try works.
            sleep(Duration::from_secs(60 * 60)).await;
            assert_eq!(*seen.borrow(), Some(eia(0.16, "Texas", "2026-09")));
        }
        .boxed_local()
    })
    .await;

    assert_eq!(
        log.text(),
        "paddock: the host's electricity price goes unread: api.eia.gov: the service did not answer in time\n"
    );
}

#[tokio::test(start_paused = true)]
async fn no_key_and_no_price_is_unpriced() {
    let web = FakeWeb::texas();
    let log = Log::default();

    // Neither a price nor a key variable.
    let none = keeping("", &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(WEEK).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    // A key variable the dog's environment does not have.
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");
    let unset = keeping(&table, &web, no_environment, &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    assert_eq!((none, unset), (None, None));
    assert_eq!(
        web.asked(),
        Vec::<Url>::new(),
        "a lookup was made without a key"
    );
    assert_eq!(
        log.text(),
        "paddock: the host's electricity price goes unread: the key variable is unset or empty\n"
    );
}

#[tokio::test(start_paused = true)]
async fn the_key_is_read_when_a_lookup_needs_it() {
    let web = FakeWeb::texas();
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");
    let reads = Rc::new(RefCell::new(0_u32));
    let counted = Rc::clone(&reads);
    let env = move |name: &str| {
        *counted.borrow_mut() += 1;
        with_key(KEY_VARIABLE)(name)
    };

    keeping(&table, &web, env, &log, |_| {
        async move { sleep(2 * WEEK + Duration::from_secs(60)).await }.boxed_local()
    })
    .await;

    // Once at the start, and once at each of the lookups a week and two weeks later.
    assert_eq!(*reads.borrow(), 3);
}

#[tokio::test(start_paused = true)]
async fn the_key_never_reaches_a_log_or_status() {
    let reasons = [
        FetchError::Unreachable,
        FetchError::TimedOut,
        FetchError::Refused(401),
        FetchError::Unreadable,
    ];
    let log = Log::default();
    let table = format!("eia_key_env = \"{KEY_VARIABLE}\"");
    for reason in reasons {
        let web =
            FakeWeb::answering("ipinfo.io", vec![Ok(IPINFO)]).and("api.eia.gov", vec![Err(reason)]);
        keeping(&table, &web, with_key(KEY_VARIABLE), &log, |_| {
            async move { sleep(Duration::from_secs(1)).await }.boxed_local()
        })
        .await;
    }
    // A lookup that succeeds with the key in play, for the status to show.
    let web = FakeWeb::texas();
    let priced = keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;
    let status = price_body(priced.as_ref()).to_string();
    assert!(status.contains("\"eia\""), "{status}");
    assert!(!status.contains(KEY), "{status}");
    // A 200 with a body that is no price at all.
    let web = FakeWeb::answering("ipinfo.io", vec![Ok(IPINFO)]).and("api.eia.gov", vec![Ok("{}")]);
    let shown = keeping(&table, &web, with_key(KEY_VARIABLE), &log, |seen| {
        async move {
            sleep(Duration::from_secs(1)).await;
            seen.borrow().clone()
        }
        .boxed_local()
    })
    .await;

    assert_eq!(log.text().lines().count(), 5, "{}", log.text());
    assert!(!log.text().contains(KEY), "{}", log.text());
    assert_eq!(shown, None);
    assert!(!format!("{priced:?}").contains(KEY));
}

/// The one place a url with the key in it meets an error is the real client's, so it is the one
/// that has to leave the url out.
#[tokio::test]
async fn a_refused_connection_names_no_url() {
    // Port 1 is tcpmux, which nothing on a host of this dog's runs.
    let url = Url::parse(&format!("http://127.0.0.1:1/data?api_key={KEY}")).expect("url");
    let web = Web::new(crate::outbound::http_client());

    let refused = tokio::time::timeout(Duration::from_secs(10), web.get(&url))
        .await
        .expect("an answer");

    assert_eq!(refused, Err(FetchError::Unreachable));
    assert!(!format!("{refused:?}").contains(KEY));
}
