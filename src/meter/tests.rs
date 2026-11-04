//! Kasa's wire against a device on loopback, the parts' sums, and the reporter that skips what it
//! cannot read.

use core::time::Duration;
use std::{
    cell::RefCell,
    io,
    sync::{Arc, Mutex},
};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use shep_client::dogs::Stop;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, duplex},
    time::timeout,
};

use super::{
    Kasa, Meter, Parts, ReadError, Reading, Source,
    kasa::{Duplex, Link, decrypt, encrypt, frame},
    report,
};
use crate::{
    metrics::{Channel, Kind, Sample},
    test_support::{FakeMeter, FakeSensors},
};

const SYSINFO: &str = include_str!("testdata/hs300_sysinfo.json");
const HS300_REALTIME: &str = include_str!("testdata/hs300_realtime.json");
const KP115_REALTIME: &str = include_str!("testdata/kp115_realtime.json");
const SMI_POWER_DRAW: &str = include_str!("testdata/nvidia_smi_power_draw.txt");

/// The request a strip is sent for its fourth outlet, and what the cipher makes of it with its
/// length in front. The bytes come from an independent XOR autokey implementation, not from
/// [`encrypt`].
const OUTLET_REQUEST: &str = r#"{"context":{"child_ids":["8006A1B2C3D4E5F60718293A4B5C6D7E8F90123403"]},"emeter":{"get_realtime":{}}}"#;
const OUTLET_REQUEST_HEX: &str = "00000065d0f291fe90e481f98daf95eeccafc7aec2a6f990f487a59fc4e6deeedee8a998dae8ab98dce8ad98dee8d8efdee6d4edde9fabe9dc9fa9edda9fa7e1d8e8d9ebd8ecdcefcd90edc1e386eb8efa9fedcff58eaccbaeda85f792f39feb82ef8aa892e994e994";

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect()
}

fn almost(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got} is not {want}");
}

/// A Kasa device that answers the first of `answers` whose needle a request holds, and holds
/// the connection open, silent, for a request that holds none
#[derive(Debug, Clone)]
struct Device {
    answers: &'static [(&'static str, &'static str)],
    asked: Arc<Mutex<Vec<String>>>,
}

impl Device {
    fn answering(answers: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            answers,
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("lock").clone()
    }

    fn kasa(&self, outlet: Option<u8>, limit: Duration) -> Kasa {
        Kasa::new(Box::new(self.clone()), outlet, limit)
    }
}

impl Link for Device {
    fn open(&self) -> LocalBoxFuture<'_, io::Result<Box<dyn Duplex>>> {
        let (client, mut server) = duplex(8192);
        let device = self.clone();
        tokio::spawn(async move {
            let mut length = [0; 4];
            server.read_exact(&mut length).await.expect("length");
            let mut request = vec![0; u32::from_be_bytes(length) as usize];
            server.read_exact(&mut request).await.expect("request");
            let request = String::from_utf8(decrypt(&request)).expect("utf8");
            let answer = device
                .answers
                .iter()
                .find(|(needle, _)| request.contains(needle))
                .map(|(_, answer)| *answer);
            device.asked.lock().expect("lock").push(request);
            match answer {
                Some(answer) => server.write_all(&frame(answer)).await.expect("answer"),
                None => core::future::pending::<()>().await,
            }
        });
        async move { Ok(Box::new(client) as Box<dyn Duplex>) }.boxed_local()
    }
}

/// A device nothing answers: the connection is refused
#[derive(Debug)]
struct Refusing;

impl Link for Refusing {
    fn open(&self) -> LocalBoxFuture<'_, io::Result<Box<dyn Duplex>>> {
        async { Err(io::ErrorKind::ConnectionRefused.into()) }.boxed_local()
    }
}

const FIVE_SECONDS: Duration = Duration::from_secs(5);

#[tokio::test(start_paused = true)]
async fn an_hs300_outlets_reading_is_its_watts() {
    let device = Device::answering(&[("get_sysinfo", SYSINFO), ("get_realtime", HS300_REALTIME)]);

    let read = timeout(FIVE_SECONDS, device.kasa(Some(3), FIVE_SECONDS).read()).await;

    assert_eq!(
        read,
        Ok(Ok(Reading {
            watts: 126.842,
            source: Source::Wall
        }))
    );
    assert_eq!(
        device.asked(),
        [r#"{"system":{"get_sysinfo":{}}}"#, OUTLET_REQUEST]
    );
}

#[tokio::test(start_paused = true)]
async fn a_plug_that_answers_power_in_watts_reads_the_same() {
    let device = Device::answering(&[("get_realtime", KP115_REALTIME)]);

    let read = timeout(FIVE_SECONDS, device.kasa(None, FIVE_SECONDS).read()).await;

    assert_eq!(
        read,
        Ok(Ok(Reading {
            watts: 49.32,
            source: Source::Wall
        }))
    );
    assert_eq!(device.asked(), [r#"{"emeter":{"get_realtime":{}}}"#]);
}

#[test]
fn the_cipher_round_trips_a_recorded_request() {
    let recorded = unhex(OUTLET_REQUEST_HEX);

    assert_eq!(frame(OUTLET_REQUEST), recorded);
    assert_eq!(decrypt(&recorded[4..]), OUTLET_REQUEST.as_bytes());
    assert_eq!(decrypt(&encrypt(b"")), b"");
}

#[tokio::test(start_paused = true)]
async fn a_strip_without_the_outlet_reads_nothing() {
    let device = Device::answering(&[("get_sysinfo", SYSINFO)]);

    let read = timeout(FIVE_SECONDS, device.kasa(Some(6), FIVE_SECONDS).read()).await;

    assert_eq!(read, Ok(Err(ReadError::NoOutlet)));
}

#[tokio::test(start_paused = true)]
async fn a_plug_that_never_answers_times_out() {
    let device = Device::answering(&[]);

    let read = timeout(
        FIVE_SECONDS,
        device.kasa(None, Duration::from_millis(100)).read(),
    )
    .await;

    assert_eq!(read, Ok(Err(ReadError::TimedOut)));
}

#[tokio::test(start_paused = true)]
async fn a_plug_that_refuses_the_connection_is_unreachable() {
    let kasa = Kasa::new(Box::new(Refusing), None, FIVE_SECONDS);

    let read = timeout(FIVE_SECONDS, kasa.read()).await;

    assert_eq!(read, Ok(Err(ReadError::Unreachable)));
}

#[tokio::test(start_paused = true)]
async fn an_answer_with_an_error_code_is_not_a_reading() {
    let device = Device::answering(&[(
        "get_realtime",
        r#"{"emeter":{"get_realtime":{"err_code":-1}}}"#,
    )]);

    let read = timeout(FIVE_SECONDS, device.kasa(None, FIVE_SECONDS).read()).await;

    assert_eq!(read, Ok(Err(ReadError::Unreadable)));
}

fn parts(printed: &str, counts: &[u64], range: u64) -> Parts {
    Parts::new(Box::new(FakeSensors::reading(printed, counts, range)))
}

/// The counter's two reads are half a second apart on the paused clock, so 50 J between them is
/// 100 W.
#[tokio::test(start_paused = true)]
async fn parts_add_the_gpu_draw_and_the_cpu_packages_power() {
    let parts = parts(SMI_POWER_DRAW, &[1_000_000, 51_000_000], 262_143_328_850);

    let reading = parts.read().await.expect("a reading");

    assert_eq!(reading.source, Source::Parts);
    almost(reading.watts, 287.45 + 100.0);
}

#[tokio::test(start_paused = true)]
async fn a_wrapped_rapl_counter_still_reads_its_power() {
    // 25 J before the wrap, and 25 J after it.
    let parts = parts(
        SMI_POWER_DRAW,
        &[262_118_328_850, 25_000_000],
        262_143_328_850,
    );

    let reading = parts.read().await.expect("a reading");

    almost(reading.watts, 287.45 + 100.0);
}

#[tokio::test(start_paused = true)]
async fn every_gpu_on_the_host_is_added() {
    let parts = parts("100.50\n200.25\n", &[0, 0], 1_000_000);

    let reading = parts.read().await.expect("a reading");

    almost(reading.watts, 300.75);
}

#[tokio::test(start_paused = true)]
async fn parts_without_a_gpu_read_nothing_rather_than_the_cpu_alone() {
    let sensors = FakeSensors::reading("", &[0, 1_000_000], 1_000_000).without_gpu();

    let read = Parts::new(Box::new(sensors)).read().await;

    assert_eq!(read, Err(ReadError::NoGpu));
}

#[tokio::test(start_paused = true)]
async fn a_gpu_the_driver_cannot_read_is_not_zero_watts() {
    let read = parts("[N/A]\n", &[0, 1_000_000], 1_000_000).read().await;

    assert_eq!(read, Err(ReadError::NoGpu));
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_counter_reads_nothing_rather_than_the_gpu_alone() {
    let read = parts(SMI_POWER_DRAW, &[], 1_000_000).read().await;

    assert_eq!(read, Err(ReadError::NoCounter));
}

/// A channel that records what it is sent
#[derive(Default)]
struct Sink {
    sent: RefCell<Vec<Sample>>,
}

impl Sink {
    fn values(&self, name: &str) -> Vec<f64> {
        let sent = self.sent.borrow();
        sent.iter()
            .filter(|sample| sample.series.name == name)
            .map(|sample| sample.value)
            .collect()
    }
}

impl Channel for Sink {
    fn is_active(&self) -> bool {
        true
    }

    fn send(&self, sample: &Sample) {
        self.sent.borrow_mut().push(sample.clone());
    }
}

fn wall(watts: f64) -> Result<Reading, ReadError> {
    Ok(Reading {
        watts,
        source: Source::Wall,
    })
}

/// Runs the reporter over `answers`, a read every five seconds, until `secs` have passed
async fn reported(answers: Vec<Result<Reading, ReadError>>, secs: u64) -> (Sink, String) {
    let meter = FakeMeter::answering(answers);
    let sink = Sink::default();
    let mut log = Vec::new();
    let (stop, _request) = Stop::new();
    let _ = timeout(
        Duration::from_secs(secs),
        report(&meter, FIVE_SECONDS, &sink, stop, &mut log),
    )
    .await;
    (sink, String::from_utf8(log).expect("utf8"))
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_is_skipped_not_zero() {
    let down = Err(ReadError::Unreachable);
    let answers = vec![wall(100.0), down, down, wall(200.0)];

    let (sink, log) = reported(answers, 16).await;

    assert_eq!(sink.values("paddock_power_watts"), [100.0, 200.0]);
    // 200 W over the fifteen seconds since the last good reading.
    assert_eq!(sink.values("paddock_energy_joules_total"), [0.0, 3000.0]);
    assert_eq!(
        log,
        "paddock: the host's power goes unread: the plug refused or dropped the connection\n"
    );
}

#[tokio::test(start_paused = true)]
async fn a_reason_that_comes_back_after_a_good_read_is_logged_again() {
    let down = Err(ReadError::TimedOut);
    let answers = vec![down, wall(100.0), down];

    let (_, log) = reported(answers, 11).await;

    assert_eq!(log.lines().count(), 2, "{log}");
}

#[tokio::test(start_paused = true)]
async fn energy_adds_watts_times_the_seconds_since_the_last_good_reading() {
    let answers = vec![wall(100.0), wall(100.0), wall(300.0)];

    let (sink, log) = reported(answers, 11).await;

    assert_eq!(
        sink.values("paddock_energy_joules_total"),
        [0.0, 500.0, 2000.0]
    );
    assert_eq!(log, "");
}

#[tokio::test(start_paused = true)]
async fn the_reporter_is_a_series_of_kinds_the_metrics_dog_knows() {
    let (sink, _) = reported(vec![wall(10.0)], 1).await;

    let kinds: Vec<Kind> = sink.sent.borrow().iter().map(|s| s.series.kind).collect();
    assert_eq!(kinds, [Kind::Gauge, Kind::Counter]);
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_the_reporter() {
    let meter = FakeMeter::default();
    let sink = Sink::default();
    let (stop, request) = Stop::new();
    request.request();

    let ended = timeout(
        FIVE_SECONDS,
        report(&meter, FIVE_SECONDS, &sink, stop, Vec::new()),
    )
    .await;

    assert!(ended.is_ok(), "the reporter outlived the stop");
}

#[test]
fn a_kasa_host_is_read_from_the_plug_and_none_from_the_parts() {
    use crate::{config::MeterConfig, survey::probe::NvidiaSmi};

    let settings = |kasa: Option<&str>| MeterConfig {
        kasa: kasa.map(str::to_owned),
        outlet: None,
        every: FIVE_SECONDS,
    };

    let plug = super::from_config(&settings(Some("plug.lan")), NvidiaSmi::default());
    let counters = super::from_config(&settings(None), NvidiaSmi::default());

    assert!(format!("{plug:?}").starts_with("Kasa"));
    assert!(format!("{counters:?}").starts_with("Parts"));
}
