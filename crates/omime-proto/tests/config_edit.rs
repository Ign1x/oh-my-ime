//! Configuration edits and hotkey validation.

use omime_proto::config::{self, Config, HotkeySpec, Value};

#[test]
fn nul_in_hotkey_is_rejected_before_c_string_conversion() {
    // TOML permits escaped NUL, but xkbcommon cannot accept it in a keysym name.
    let (config, _) = Config::parse("[hotkeys]\ntoggle = \"Control+\\u0000\"\n").unwrap();
    assert!(HotkeySpec::parse(&config.hotkeys.toggle).is_err());
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
