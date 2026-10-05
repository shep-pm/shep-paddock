//! `shep paddock status`: the status endpoint as a table.

use std::{io::Write, time::Duration};

use reqwest::Method;
use serde_json::Value;

use super::Link;
use crate::outbound::http_client;

// A status answers from the engine's memory; ten seconds is a dog that is not answering.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How a cell shows a value the dog left empty
const EMPTY: &str = "-";

fn cell(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => EMPTY.to_owned(),
    }
}

/// One titled table, columns two spaces apart, or `(none)` for no rows
fn section(title: &str, rows: &Value, columns: &[(&str, &str)]) -> String {
    let rows: Vec<Vec<String>> = rows
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| columns.iter().map(|(_, key)| cell(row, key)).collect())
                .collect()
        })
        .unwrap_or_default();
    let mut text = format!("{title}\n");
    if rows.is_empty() {
        text.push_str("(none)\n");
        return text;
    }
    let headers: Vec<String> = columns.iter().map(|(name, _)| (*name).to_owned()).collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|at| {
            rows.iter()
                .chain([&headers])
                .map(|row| row[at].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in [&headers].into_iter().chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(text, width)| format!("{text:<width$}"))
            .collect();
        text.push_str(line.join("  ").trim_end());
        text.push('\n');
    }
    text
}

/// The status as text
pub(crate) fn render(status: &Value) -> String {
    let models = section(
        "models",
        &status["models"],
        &[
            ("MODEL", "model"),
            ("STATE", "state"),
            ("IN-FLIGHT", "in_flight"),
            ("HELD-BY", "held_by"),
            ("LAST-USED", "last_used"),
        ],
    );
    let leases = section(
        "leases",
        &status["leases"],
        &[
            ("ID", "id"),
            ("CLIENT", "client"),
            ("MODEL", "model"),
            ("HOLD", "hold"),
            ("SINCE", "since"),
            ("EXPECTED-UNTIL", "expected_until"),
            ("NOTE", "note"),
        ],
    );
    let waiters = section(
        "waiters",
        &status["waiters"],
        &[
            ("CLIENT", "client"),
            ("MODEL", "model"),
            ("KIND", "kind"),
            ("PRIORITY", "priority"),
            ("SINCE", "since"),
            ("REASON", "reason"),
        ],
    );
    format!("{models}\n{leases}\n{waiters}")
}

/// Fetches the status and prints it
pub(crate) async fn status(link: &Link, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let client = http_client();
    let sent = link
        .request(&client, Method::GET, "/paddock/status")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(failure) => {
            let _ = writeln!(
                err,
                "paddock: cannot reach the dog at {}: {failure}",
                link.url
            );
            return 1;
        }
    };
    let code = response.status();
    let body = response.text().await.unwrap_or_default();
    if !code.is_success() {
        let _ = writeln!(err, "paddock: the dog answered {code}: {body}");
        return 1;
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(status) => {
            let _ = out.write_all(render(&status).as_bytes());
            0
        }
        Err(failure) => {
            let _ = writeln!(err, "paddock: the status is not JSON: {failure}");
            1
        }
    }
}

#[cfg(test)]
mod tests;
