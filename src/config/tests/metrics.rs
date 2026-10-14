//! A model's `metrics`, which says its backend's own numbers are read for the metrics dog.

use super::*;

#[test]
fn metrics_strata_is_accepted_on_a_sheep_model() {
    let config = Config::from_toml(&with_iq3_s("metrics = \"strata\"")).unwrap();

    assert_eq!(
        config.models[&name("iq3_s")].metrics,
        Some(MetricsKind::Strata)
    );
    assert_eq!(config.models[&name("laya")].metrics, None);
}

#[test]
fn metrics_on_an_ollama_model_is_refused() {
    let text = two_ollama_models("qwen3", "ollama")
        .replace("[models.q]", "[models.q]\nmetrics = \"strata\"");

    assert_eq!(
        Config::from_toml(&text).map(|_| ()),
        Err(ConfigError::MetricsOnOllama { model: name("q") })
    );
}

#[test]
fn an_unknown_metrics_kind_is_refused_naming_models_name_metrics() {
    let Err(ConfigError::Toml(shown)) = Config::from_toml(&with_iq3_s("metrics = \"prom\""))
    else {
        panic!("a metrics kind that is not strata is refused");
    };

    assert!(
        shown.starts_with("unknown variant `prom`, expected `strata` in `models.iq3_s.metrics`"),
        "{shown}"
    );
}

#[test]
fn the_schema_accepts_strata_for_metrics() {
    let schema = shep_client::dogs::config_schema::<section::Section>();
    let value = schema.as_value();
    let reference = value
        .pointer("/$defs/ModelSection/properties/metrics/anyOf/0/$ref")
        .and_then(serde_json::Value::as_str);
    assert_eq!(reference, Some("#/$defs/MetricsKind"));
    let kinds = value
        .pointer("/$defs/MetricsKind/oneOf")
        .and_then(serde_json::Value::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(|kind| kind.get("const")?.as_str())
                .collect::<Vec<_>>()
        });
    assert_eq!(kinds, Some(vec!["strata"]));
}

#[test]
fn changing_metrics_does_not_change_the_launch_settings() {
    let plain = Config::from_toml(&with_iq3_s("")).unwrap();
    let marked = Config::from_toml(&with_iq3_s("metrics = \"strata\"")).unwrap();
    let launch = |config: &Config| config.models[&name("iq3_s")].launch_in(None);

    assert_eq!(launch(&plain), launch(&marked));
    assert!(launch(&plain).is_some());
}

#[test]
fn a_models_debug_shows_its_metrics() {
    let config = Config::from_toml(&with_iq3_s("metrics = \"strata\"")).unwrap();
    let shown = format!("{:?}", config.models[&name("iq3_s")]);
    assert!(shown.contains("metrics: Some(Strata)"), "{shown}");
}
