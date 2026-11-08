//! Configuration edits and hotkey validation.

use omime_proto::config::{self, Config, HotkeySpec, Value};

#[test]
fn nul_in_hotkey_is_rejected_before_c_string_conversion() {
    // TOML permits escaped NUL, but xkbcommon cannot accept it in a keysym name.
    assert!(HotkeySpec::parse("Control+\0").is_err());
}

#[test]
fn nul_in_each_exposed_config_string_is_rejected_without_echoing_its_value() {
    for (key, text, value) in [
        (
            "engines",
            "engines = ['echo', \"private\\u0000value\"]",
            Value::List(vec!["echo".into(), "private\0value".into()]),
        ),
        (
            "initial_input_method",
            "initial_input_method = \"private\\u0000value\"",
            Value::String("private\0value".into()),
        ),
        (
            "hotkeys.toggle",
            "[hotkeys]\ntoggle = \"private\\u0000value\"",
            Value::String("private\0value".into()),
        ),
        (
            "hotkeys.next",
            "[hotkeys]\nnext = \"private\\u0000value\"",
            Value::String("private\0value".into()),
        ),
    ] {
        for error in [
            Config::parse(text).expect_err(key),
            config::edit("", key, value).expect_err(key),
        ] {
            assert!(error.contains(&format!("`{key}`")), "{error}");
            assert!(!error.contains("private"), "{error}");
            assert!(!error.contains('\0'), "{error:?}");
        }
    }
}

#[test]
fn unicode_config_strings_still_parse_and_edit() {
    let text = "engines = ['echo', 'rime/拼音']\ninitial_input_method = 'rime/拼音'\n\
                [hotkeys]\ntoggle = 'Control+é'\nnext = 'Control+あ'\n";
    let (config, unknown) = Config::parse(text).unwrap();
    assert!(unknown.is_empty());
    assert_eq!(config.engines.unwrap(), ["echo", "rime/拼音"]);
    assert_eq!(config.initial_input_method.as_deref(), Some("rime/拼音"));
    assert_eq!(config.hotkeys.toggle, "Control+é");
    assert_eq!(config.hotkeys.next, "Control+あ");
    let edited = config::edit(text, "hotkeys.next", Value::String("Control+ß".into())).unwrap();
    assert_eq!(Config::parse(&edited).unwrap().0.hotkeys.next, "Control+ß");
}

#[test]
fn control_characters_in_hotkeys_are_rejected() {
    for control in ('\0'..='\u{1f}').chain('\u{7f}'..='\u{9f}') {
        for text in [
            control.to_string(),
            format!("Control+{control}"),
            format!("Control+{control}space"),
            format!("Control+sp{control}ace"),
            format!("Control+space{control}"),
        ] {
            assert!(HotkeySpec::parse(&text).is_err(), "{text:?}");
            for key in ["hotkeys.toggle", "hotkeys.next"] {
                assert!(
                    config::edit("", key, Value::String(text.clone())).is_err(),
                    "{key} = {text:?}"
                );
            }
        }
    }
}

#[test]
fn editing_a_mistyped_section_returns_an_error_without_panicking() {
    for key in config::KEYS {
        let Some((section, _)) = key.split_once('.') else {
            continue;
        };
        let value = match section {
            "hotkeys" => Value::String("Control+space".into()),
            "engine" => Value::Integer(300),
            _ => Value::Bool(true),
        };
        let malformed = ["false", "'oops'", "1", "[]", "\"rime\""]
            .map(|value| format!("{section} = {value}\n"));
        for text in malformed.into_iter().chain([format!("[[{section}]]\n")]) {
            let result = std::panic::catch_unwind(|| config::edit(&text, key, value.clone()));
            assert!(result.is_ok(), "editing {key} panicked on {text:?}");
            let error = result
                .unwrap()
                .expect_err("invalid section must be rejected");
            assert!(error.contains(&format!("`{section}`")), "{error}");
        }
    }
}

#[test]
fn inline_and_dotted_tables_are_edited_in_place_with_comments() {
    for text in [
        "# my settings\napps = { remember = false } # applications\n# keep this too\n",
        "# my settings\napps.remember = false # applications\n# keep this too\n",
    ] {
        let edited = config::edit(text, "apps.remember", Value::Bool(true)).unwrap();
        assert_eq!(edited, text.replace("false", "true"));
        assert!(Config::parse(&edited).unwrap().0.apps.remember);
    }
}
