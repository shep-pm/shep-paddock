//! TP-Link Kasa's local protocol, for a plug or strip with an energy meter.
//!
//! One request per connection on port 9999: a four-byte big-endian length, then JSON under an XOR
//! autokey cipher that starts from [`KEY`]. A strip is asked for one outlet through `child_ids`,
//! whose ids are the strip's device id followed by the outlet's two digits.

use core::{cell::RefCell, fmt, time::Duration};
use std::io;

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use super::{Meter, ReadError, Reading, Source};

/// The port a Kasa device answers on
pub(crate) const PORT: u16 = 9999;
/// How long a read has to connect, ask and be answered
pub(crate) const LIMIT: Duration = Duration::from_secs(1);

/// The cipher's first key
const KEY: u8 = 171;
/// The longest answer read. A strip's `get_sysinfo` runs to a few kilobytes.
const MAX_ANSWER: usize = 64 * 1024;

/// A connection to a device
pub(crate) trait Duplex: AsyncRead + AsyncWrite + Unpin {}

impl<T: AsyncRead + AsyncWrite + Unpin> Duplex for T {}

/// How a connection to the device is made
///
/// Boxed futures so it can sit behind `dyn`.
pub(crate) trait Link: fmt::Debug {
    /// A new connection.
    fn open(&self) -> LocalBoxFuture<'_, io::Result<Box<dyn Duplex>>>;
}

/// TCP to a host's Kasa port
#[derive(Debug)]
pub(crate) struct Tcp {
    host: String,
    port: u16,
}

impl Tcp {
    /// The device at `host`, on `port`
    pub(crate) fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }
}

impl Link for Tcp {
    fn open(&self) -> LocalBoxFuture<'_, io::Result<Box<dyn Duplex>>> {
        async {
            let stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
            Ok(Box::new(stream) as Box<dyn Duplex>)
        }
        .boxed_local()
    }
}

/// A Kasa plug, or one outlet of a strip
#[derive(Debug)]
pub(crate) struct Kasa {
    link: Box<dyn Link>,
    outlet: Option<u8>,
    limit: Duration,
    /// The strip's outlet id, found once from its `get_sysinfo`.
    child: RefCell<Option<String>>,
}

impl Kasa {
    /// A meter on the device `link` reaches, on `outlet` if it is a strip, giving up after `limit`
    pub(crate) fn new(link: Box<dyn Link>, outlet: Option<u8>, limit: Duration) -> Self {
        Self {
            link,
            outlet,
            limit,
            child: RefCell::new(None),
        }
    }

    async fn watts(&self) -> Result<f64, ReadError> {
        let request = match self.outlet {
            None => json!({"emeter": {"get_realtime": {}}}),
            Some(outlet) => {
                let child = self.child_id(outlet).await?;
                json!({"context": {"child_ids": [child]}, "emeter": {"get_realtime": {}}})
            }
        };
        let answer = self.ask(&request).await?;
        watts_in(&answer).ok_or(ReadError::Unreadable)
    }

    /// The id the strip knows `outlet` by
    async fn child_id(&self, outlet: u8) -> Result<String, ReadError> {
        if let Some(child) = &*self.child.borrow() {
            return Ok(child.clone());
        }
        let answer = self.ask(&json!({"system": {"get_sysinfo": {}}})).await?;
        let child = child_id_in(&answer, outlet)?;
        *self.child.borrow_mut() = Some(child.clone());
        Ok(child)
    }

    async fn ask(&self, request: &Value) -> Result<Value, ReadError> {
        let unreachable = |_: io::Error| ReadError::Unreachable;
        let mut stream = self.link.open().await.map_err(unreachable)?;
        stream
            .write_all(&frame(&request.to_string()))
            .await
            .map_err(unreachable)?;
        let mut length = [0; 4];
        stream.read_exact(&mut length).await.map_err(unreachable)?;
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if length > MAX_ANSWER {
            return Err(ReadError::Unreadable);
        }
        let mut answer = vec![0; length];
        stream.read_exact(&mut answer).await.map_err(unreachable)?;
        serde_json::from_slice(&decrypt(&answer)).map_err(|_| ReadError::Unreadable)
    }
}

impl Meter for Kasa {
    fn read(&self) -> LocalBoxFuture<'_, Result<Reading, ReadError>> {
        async {
            let watts = timeout(self.limit, self.watts())
                .await
                .map_err(|_| ReadError::TimedOut)??;
            Ok(Reading {
                watts,
                source: Source::Wall,
            })
        }
        .boxed_local()
    }
}

/// The watts in a `get_realtime` answer: `power_mw` on some firmware, `power` on others
fn watts_in(answer: &Value) -> Option<f64> {
    let realtime = answer.pointer("/emeter/get_realtime")?;
    if realtime.get("err_code").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let watts = match realtime.get("power_mw").and_then(Value::as_f64) {
        Some(milliwatts) => milliwatts / 1000.0,
        None => realtime.get("power").and_then(Value::as_f64)?,
    };
    watts.is_finite().then_some(watts)
}

/// The id of `outlet` on the strip a `get_sysinfo` answer describes
///
/// Each child's id is the device id and its two-digit outlet number, so the device id is
/// whatever the first one has before them.
fn child_id_in(answer: &Value, outlet: u8) -> Result<String, ReadError> {
    let children = answer
        .pointer("/system/get_sysinfo/children")
        .and_then(Value::as_array)
        .ok_or(ReadError::Unreadable)?;
    if usize::from(outlet) >= children.len() {
        return Err(ReadError::NoOutlet);
    }
    let first = children
        .first()
        .and_then(|child| child.get("id"))
        .and_then(Value::as_str)
        .and_then(|id| id.len().checked_sub(2).and_then(|end| id.get(..end)))
        .ok_or(ReadError::Unreadable)?;
    Ok(format!("{first}{outlet:02}"))
}

/// `request` as it goes on the wire
pub(super) fn frame(request: &str) -> Vec<u8> {
    let cipher = encrypt(request.as_bytes());
    // A request is a few dozen bytes.
    let length = u32::try_from(cipher.len()).unwrap_or(u32::MAX);
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend(cipher);
    framed
}

/// `plain` under the autokey cipher: each byte is XORed with the one before it, enciphered
pub(super) fn encrypt(plain: &[u8]) -> Vec<u8> {
    let mut key = KEY;
    plain
        .iter()
        .map(|byte| {
            key ^= byte;
            key
        })
        .collect()
}

/// What [`encrypt`] made `cipher` from
pub(super) fn decrypt(cipher: &[u8]) -> Vec<u8> {
    let mut key = KEY;
    cipher
        .iter()
        .map(|byte| {
            let plain = key ^ byte;
            key = *byte;
            plain
        })
        .collect()
}
