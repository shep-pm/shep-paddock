use super::*;

#[test]
fn a_span_inside_a_multibyte_character_is_not_located() {
    let Err(err) = toml::from_str::<Section>("listen = 1\n") else {
        panic!("a number is not a listen address");
    };
    let start = err.span().expect("a span").start;
    // `é` is two bytes, so an odd offset into a run of them falls inside one.
    let pad = if start % 2 == 1 { "" } else { "a" };
    let text = format!("{pad}{}", "é".repeat(start + 2));

    let located = ConfigError::from_toml_error(&err, &text);

    assert!(
        matches!(&located, ConfigError::Toml(shown) if shown == "a value of the wrong type or form"),
        "{located:?}"
    );
}

#[test]
fn an_error_can_be_cloned_and_compared_whole() {
    let text = MINIMAL.replace(r#"idle = "8h""#, r#"idle = "8 hours""#);
    let err = Config::from_toml(&text).unwrap_err();
    assert_eq!(err.clone(), err);
    assert_ne!(err, Config::from_toml("listen = 1").unwrap_err());
}

#[test]
fn the_column_counts_characters_not_bytes() {
    let text = "host = { vram = \"ééé\", ramm = \"1G\" }\n";
    let column = text[..text.find("ramm").unwrap()].chars().count() + 1;
    let Err(ConfigError::Toml(shown)) = Config::from_toml(text) else {
        panic!("a misspelled key is refused");
    };
    assert!(
        shown.ends_with(&format!("at line 1, column {column}")),
        "{shown}"
    );
}
