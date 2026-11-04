//! The `[meter]` table, which says where the host's power is read.

use super::*;
use crate::config::meter::UsdPerKwh;

fn with_meter(table: &str) -> String {
    format!("{MINIMAL}\n[meter]\n{table}")
}

#[test]
fn a_section_without_a_meter_reads_nothing() {
    assert_eq!(Config::from_toml(MINIMAL).unwrap().meter, None);
}

#[test]
fn an_empty_meter_table_reads_the_parts_every_five_seconds() {
    let meter = Config::from_toml(&with_meter("")).unwrap().meter;

    assert_eq!(
        meter,
        Some(MeterConfig {
            kasa: None,
            outlet: None,
            every: Duration::from_secs(5),
            usd_per_kwh: None,
            eia_key_env: None,
        })
    );
}

#[test]
fn a_kasa_outlet_and_interval_are_read_as_written() {
    let text = with_meter("kasa = \"192.168.1.40\"\noutlet = 3\nevery = \"30s\"");

    let meter = Config::from_toml(&text).unwrap().meter;

    assert_eq!(
        meter,
        Some(MeterConfig {
            kasa: Some("192.168.1.40".to_owned()),
            outlet: Some(3),
            every: Duration::from_secs(30),
            usd_per_kwh: None,
            eia_key_env: None,
        })
    );
}

#[test]
fn an_outlet_without_a_kasa_is_refused() {
    assert_eq!(
        Config::from_toml(&with_meter("outlet = 1")).map(|_| ()),
        Err(ConfigError::MeterOutletWithoutKasa)
    );
}

#[test]
fn an_outlet_past_the_hs300s_sixth_is_refused() {
    assert_eq!(
        Config::from_toml(&with_meter("kasa = \"plug\"\noutlet = 6")).map(|_| ()),
        Err(ConfigError::MeterOutletOutOfRange { outlet: 6 })
    );
}

#[test]
fn the_outlets_on_both_ends_of_the_strip_are_accepted() {
    for outlet in [0, 5] {
        let text = with_meter(&format!("kasa = \"plug\"\noutlet = {outlet}"));
        assert!(Config::from_toml(&text).is_ok(), "outlet {outlet}");
    }
}

#[test]
fn an_empty_kasa_host_is_refused() {
    assert_eq!(
        Config::from_toml(&with_meter("kasa = \"\"")).map(|_| ()),
        Err(ConfigError::EmptyMeterHost)
    );
}

#[test]
fn a_zero_interval_is_refused() {
    assert_eq!(
        Config::from_toml(&with_meter("every = \"0s\"")).map(|_| ()),
        Err(ConfigError::ZeroMeterInterval)
    );
}

#[test]
fn an_interval_shep_does_not_spell_is_refused_naming_its_field() {
    let Err(ConfigError::Duration { field, .. }) =
        Config::from_toml(&with_meter("every = \"5 s\""))
    else {
        panic!("a duration shep does not accept is refused");
    };

    assert_eq!(field, "meter.every");
}

#[test]
fn an_unknown_meter_key_is_refused() {
    assert!(matches!(
        Config::from_toml(&with_meter("host = \"plug\"")),
        Err(ConfigError::Toml(_))
    ));
}

#[test]
fn a_price_and_a_key_variable_are_read_as_written() {
    let text = with_meter("usd_per_kwh = 0.17\neia_key_env = \"EIA_API_KEY\"");

    let meter = Config::from_toml(&text).unwrap().meter.expect("a meter");

    assert_eq!(meter.usd_per_kwh.map(UsdPerKwh::get), Some(0.17));
    assert_eq!(meter.eia_key_env.as_deref(), Some("EIA_API_KEY"));
}

#[test]
fn a_price_may_be_written_as_a_whole_number() {
    let meter = Config::from_toml(&with_meter("usd_per_kwh = 1"))
        .unwrap()
        .meter
        .expect("a meter");

    assert_eq!(meter.usd_per_kwh.map(UsdPerKwh::get), Some(1.0));
}

#[test]
fn a_price_below_zero_or_not_a_number_is_refused() {
    for (written, shown) in [("-0.1", "-0.1"), ("nan", "NaN"), ("inf", "inf")] {
        let text = with_meter(&format!("usd_per_kwh = {written}"));
        assert_eq!(
            Config::from_toml(&text).map(|_| ()),
            Err(ConfigError::BadElectricityPrice {
                value: shown.to_owned()
            }),
            "usd_per_kwh = {written}"
        );
    }
}

#[test]
fn an_empty_key_variable_name_is_refused() {
    assert_eq!(
        Config::from_toml(&with_meter("eia_key_env = \"\"")).map(|_| ()),
        Err(ConfigError::EmptyEiaKeyEnv)
    );
}
