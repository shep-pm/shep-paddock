//! How the section's sizes, durations and urls are read and shown.

use std::time::Duration;

use shep_client::shep_core::values::{MemSize, UpDuration};

use super::ConfigError;

pub(super) fn parse_size(value: &str, field: &str) -> Result<MemSize, ConfigError> {
    value.parse().map_err(|source| ConfigError::Size {
        field: field.to_owned(),
        value: value.to_owned(),
        source,
    })
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
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..authority_end].rsplit('@').next().unwrap_or_default();
    let tail = &rest[authority_end..];
    let tail = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    match scheme {
        Some(scheme) => format!("{scheme}://{host}{tail}"),
        None => format!("{host}{tail}"),
    }
}
