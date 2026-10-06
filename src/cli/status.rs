//! `shep paddock status`: the status endpoint as a table.

use std::{io::Write, time::Duration};

use reqwest::Method;
use serde_json::{Value, json};

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

/// A byte count in the largest binary unit that keeps it at 1 or more
fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut unit = 0;
    while unit + 1 < UNITS.len() && bytes >> (10 * (unit + 1)) > 0 {
        unit += 1;
    }
    // Precision loss only blurs the second decimal of a size shown to two.
    #[allow(clippy::cast_precision_loss)]
    let scaled = bytes as f64 / (1u64 << (10 * unit)) as f64;
    let text = format!("{scaled:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} {}", UNITS[unit])
}

/// The host's totals and declared footprints, one row per resource
fn host_rows(host: &Value) -> Value {
    if !host.is_object() {
        return Value::Null;
    }
    let row = |name: &str, total: &str, declared: &str| {
        let bytes = |key: &str| host[key].as_u64().map_or_else(|| EMPTY.to_owned(), size);
        json!({ "resource": name, "total": bytes(total), "declared": bytes(declared) })
    };
    json!([
        row("vram", "vram_bytes", "vram_declared_bytes"),
        row("ram", "ram_bytes", "ram_declared_bytes"),
    ])
}

/// The status as text
pub(crate) fn render(status: &Value) -> String {
    let host = section(
        "host",
        &host_rows(&status["host"]),
        &[
            ("RESOURCE", "resource"),
            ("TOTAL", "total"),
            ("DECLARED", "declared"),
        ],
    );
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
    let errors = section(
        "errors",
        &status["errors"],
        &[("MODEL", "model"), ("AT", "at"), ("ERROR", "error")],
    );
    format!("{host}\n{models}\n{leases}\n{waiters}\n{errors}")
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
