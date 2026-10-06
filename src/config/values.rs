//! How the section's sizes, durations and urls are read and shown.

use std::time::Duration;

use shep_client::shep_core::values::{MemSize, UpDuration};

use super::ConfigError;
use crate::footprint::Vram;

pub(super) fn parse_size(value: &str, field: &str) -> Result<MemSize, ConfigError> {
    value.parse().map_err(|source| ConfigError::Size {
        field: field.to_owned(),
        value: value.to_owned(),
        source,
    })
}

/// A `vram` value: unset is none, `all` grows into whatever is free
///
/// # Errors
/// [`ConfigError::Size`] when it is neither `all` nor a size shep accepts.
pub(super) fn parse_vram(value: Option<&str>, field: &str) -> Result<Vram, ConfigError> {
    match value {
        None => Ok(Vram::None),
        Some("all") => Ok(Vram::All),
        Some(size) => Ok(Vram::Bytes(parse_size(size, field)?.bytes())),
    }
}

/// A `ram` value: unset is none
///
/// # Errors
/// [`ConfigError::Size`] when it is not a size shep accepts.
pub(super) fn parse_ram(value: Option<&str>, field: &str) -> Result<u64, ConfigError> {
    value
        .map(|size| parse_size(size, field))
        .transpose()
        .map(|size| size.map_or(0, MemSize::bytes))
}

pub(super) fn parse_duration(value: &str, field: &str) -> Result<Duration, ConfigError> {
    value
        .parse::<UpDuration>()
        .map(UpDuration::as_duration)
        .map_err(|source| ConfigError::Duration {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

pub(super) fn duration_or(
    value: Option<&str>,
    field: &str,
    default: Duration,
) -> Result<Duration, ConfigError> {
    value.map_or(Ok(default), |value| parse_duration(value, field))
}

/// `url` without its `user:password@`, query and fragment, any of which may carry a credential,
/// for an error that is logged or shown
///
/// Works on the text, so a url that does not parse, or has no scheme, is redacted too.
pub(crate) fn redacted(url: &str) -> String {
    let (lead, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (&url[..scheme.len() + 3], rest),
        None => url.strip_prefix("//").map_or(("", url), |rest| ("//", rest)),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..authority_end].rsplit('@').next().unwrap_or_default();
    let tail = &rest[authority_end..];
    let tail = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    format!("{lead}{host}{tail}")
}
