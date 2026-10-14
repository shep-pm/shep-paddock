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
        ("paddock_strata_requests_total", 3.0),
        ("paddock_strata_prompt_tokens_total", 100_137.0),
        ("paddock_strata_reused_tokens_total", 0.0),
        ("paddock_strata_output_tokens_total", 147_965.0),
        ("paddock_strata_drafts_offered_total", 131_641.0),
        ("paddock_strata_drafts_accepted_total", 93_555.0),
        ("paddock_strata_prompt_seconds_total", 34.1904),
        ("paddock_strata_decode_seconds_total", 888.506),
    ];
    for (name, want) in counters {
        let got = value(&samples, name, &[]).expect(name);
        assert!((got - want).abs() < 1e-9, "{name}: {got}");
        assert_eq!(kind(&samples, name), Some(Kind::Counter), "{name}");
    }
    let gauges = [
        ("paddock_strata_queued", 0.0),
        ("paddock_strata_tokens_per_second", 0.0),
        ("paddock_strata_gpu_utilization_percent", 0.0),
        ("paddock_strata_gpu_memory_used_bytes", 25_432_227_840.0),
        ("paddock_strata_gpu_temperature_celsius", 35.0),
        ("paddock_strata_gpu_power_watts", 11.099),
    ];
    for (name, want) in gauges {
        assert_eq!(value(&samples, name, &[]), Some(want), "{name}");
        assert_eq!(kind(&samples, name), Some(Kind::Gauge), "{name}");
    }
    for state in ["reading", "generating", "unloaded"] {
        let series = value(&samples, "paddock_strata_state", &[("state", state)]);
        assert_eq!(series, Some(0.0), "{state}");
    }
    assert_eq!(
        value(&samples, "paddock_strata_state", &[("state", "idle")]),
        Some(1.0)
    );
    // Eight counters, six gauges and the four states
    assert_eq!(samples.len(), 18, "{samples:?}");
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
