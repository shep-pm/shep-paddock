//! The reply Strata gives on `/metrics` becoming samples, and the scrape of a loaded model.

use super::*;

const CAPTURED: &str = include_str!("../testdata/strata-metrics.json");

fn model() -> ModelName {
    ModelName::from("iq3_xxs")
}

/// The samples for `body`, which the test expects to be accepted
fn read(body: &str) -> Vec<Sample> {
    samples(&model(), body.as_bytes()).expect("a reply the dog accepts")
}

/// The value of the sample named `name`, labelled by the model and by `extra`
fn value(samples: &[Sample], name: &str, extra: &[(&str, &str)]) -> Option<f64> {
    let mut want: Vec<_> = extra.to_vec();
    want.push(("model", "iq3_xxs"));
    samples
        .iter()
        .find(|sample| {
            sample.series.name == name
                && sample.labels.len() == want.len()
                && want
                    .iter()
                    .all(|(key, v)| sample.labels.get(key).map(String::as_str) == Some(*v))
        })
        .map(|sample| sample.value)
}

fn kind(samples: &[Sample], name: &str) -> Option<Kind> {
    samples
        .iter()
        .find(|sample| sample.series.name == name)
        .map(|sample| sample.series.kind)
}

#[test]
fn the_captured_reply_becomes_counters_and_gauges_labelled_by_model() {
    let samples = read(CAPTURED);

    let counters = [
        ("paddock_strata_requests_total", 412.0),
        ("paddock_strata_prompt_tokens_total", 1_843_920.0),
        ("paddock_strata_reused_tokens_total", 1_204_113.0),
        ("paddock_strata_drafts_offered_total", 90_311.0),
        ("paddock_strata_drafts_accepted_total", 61_874.0),
        ("paddock_strata_prompt_seconds_total", 913.4555),
        ("paddock_strata_decode_seconds_total", 2204.87125),
    ];
    for (name, want) in counters {
        assert_eq!(value(&samples, name, &[]), Some(want), "{name}");
        assert_eq!(kind(&samples, name), Some(Kind::Counter), "{name}");
    }
    let gauges = [
        ("paddock_strata_queued", 1.0),
        ("paddock_strata_tokens_per_second", 38.7),
        ("paddock_strata_gpu_utilization_percent", 87.0),
        ("paddock_strata_gpu_memory_used_bytes", 23_201_349_632.0),
        ("paddock_strata_gpu_temperature_celsius", 71.0),
        ("paddock_strata_gpu_power_watts", 312.4),
    ];
    for (name, want) in gauges {
        assert_eq!(value(&samples, name, &[]), Some(want), "{name}");
        assert_eq!(kind(&samples, name), Some(Kind::Gauge), "{name}");
    }
    // Seven counters, six gauges and the four states
    assert_eq!(samples.len(), 17, "{samples:?}");
}

#[test]
fn prompt_and_decode_ms_are_reported_in_seconds() {
    let samples = read(r#"{"totals": {"prompt_ms": 1500, "decode_ms": 250}}"#);

    assert_eq!(
        value(&samples, "paddock_strata_prompt_seconds_total", &[]),
        Some(1.5)
    );
    assert_eq!(
        value(&samples, "paddock_strata_decode_seconds_total", &[]),
        Some(0.25)
    );
}

#[test]
fn the_state_is_one_of_four_series_with_one_set() {
    for current in ["reading", "generating", "idle", "unloaded"] {
        let samples = read(&format!(r#"{{"live": {{"state": "{current}"}}}}"#));

        for state in ["reading", "generating", "idle", "unloaded"] {
            let want = if state == current { 1.0 } else { 0.0 };
            assert_eq!(
                value(&samples, "paddock_strata_state", &[("state", state)]),
                Some(want),
                "{state} while {current}"
            );
        }
    }
}

#[test]
fn an_unknown_state_sets_none_of_the_four() {
    let samples = read(r#"{"live": {"state": "swapping"}}"#);

    for state in ["reading", "generating", "idle", "unloaded"] {
        assert_eq!(
            value(&samples, "paddock_strata_state", &[("state", state)]),
            Some(0.0),
            "{state}"
        );
    }
}

#[test]
fn a_null_tok_s_reports_zero() {
    let samples = read(r#"{"live": {"queued": 0, "tok_s": null, "state": "idle"}}"#);

    assert_eq!(
        value(&samples, "paddock_strata_tokens_per_second", &[]),
        Some(0.0)
    );
}

#[test]
fn a_missing_or_null_field_sends_no_sample_for_it() {
    let samples = read(
        r#"{"totals": {"requests": null, "reused": 3},
            "live": {"queued": null},
            "hardware": {"gpu_util": null, "gpu_temp": 60}}"#,
    );

    assert_eq!(value(&samples, "paddock_strata_requests_total", &[]), None);
    assert_eq!(
        value(&samples, "paddock_strata_reused_tokens_total", &[]),
        Some(3.0)
    );
    assert_eq!(value(&samples, "paddock_strata_queued", &[]), None);
    assert_eq!(
        value(&samples, "paddock_strata_gpu_utilization_percent", &[]),
        None
    );
    assert_eq!(
        value(&samples, "paddock_strata_gpu_temperature_celsius", &[]),
        Some(60.0)
    );
    assert_eq!(
        value(&samples, "paddock_strata_tokens_per_second", &[]),
        None
    );
    assert_eq!(
        value(&samples, "paddock_strata_state", &[("state", "idle")]),
        None
    );
    assert_eq!(samples.len(), 2, "{samples:?}");
}

#[test]
fn an_empty_hardware_or_an_empty_reply_sends_nothing() {
    assert!(read(r#"{"hardware": {}}"#).is_empty());
    assert!(read(r#"{"hardware": null, "totals": null, "live": null}"#).is_empty());
    assert!(read("{}").is_empty());
}

#[test]
fn unknown_fields_are_ignored() {
    let samples = read(
        r#"{"totals": {"requests": 5, "something_new": [1, 2]},
            "live": {"queued": 2, "gpu_fan": 40},
            "hardware": {"gpu_util": 10, "vram_total": 1},
            "requests": [{"id": 1}], "history": [], "extra": {"a": 1}}"#,
    );

    assert_eq!(
        value(&samples, "paddock_strata_requests_total", &[]),
        Some(5.0)
    );
    assert_eq!(value(&samples, "paddock_strata_queued", &[]), Some(2.0));
    assert_eq!(samples.len(), 3, "{samples:?}");
}

#[test]
fn a_reply_that_is_not_the_json_above_is_refused() {
    for body in ["", "not json", "[]", r#"{"totals": {"requests": "many"}}"#] {
        assert!(samples(&model(), body.as_bytes()).is_err(), "{body}");
    }
}

mod scrape;
