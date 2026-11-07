//! The user configuration (`~/.config/oh-my-ime/config.toml`).
//!
//! ```toml
//! # Enabled engines, in switching order. Default: every installed engine.
//! engines = ["sparkle", "echo"]
//!
//! [hotkeys]
//! # Switch between direct input and the last used engine.
//! toggle = "Control+space"
//! # Switch to the next enabled engine.
//! next = "Control+Shift+space"
//!
//! [engine]
//! # How long to wait for an engine to answer a key before passing it through.
//! key_timeout_ms = 200
//!
//! [apps]
//! # Give each application the engine last used in it.
//! remember = true
//! # Also restore the input mode (such as Chinese/Latin) when an engine supports it.
//! remember_mode = false
//!
//! [tray]
//! # Show the current engine in the tray of status bars.
//! enabled = true
//!
//! [panel]
//! # Show the input methods for a moment when switching with the hotkeys or `omime`.
//! enabled = true
//!
//! [notifications]
//! # Tell the user about trouble: an engine that stopped, another input method on a
//! # seat, a mistake in this file.
//! enabled = true
//! ```

use crate::tr;
use serde::{Deserialize, Serialize};

/// File name of the configuration below `$XDG_CONFIG_HOME/oh-my-ime`.
pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Enabled engine ids in switching order; `None` enables everything installed.
    pub engines: Option<Vec<String>>,
    /// Used only before a saved input-method choice exists. Later logins restore
    /// the user's choice, including direct input.
    pub initial_input_method: Option<String>,
    pub hotkeys: Hotkeys,
    pub engine: EngineSettings,
    pub apps: AppSettings,
    pub tray: TraySettings,
    pub panel: PanelSettings,
    pub notifications: NotificationSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Switch between direct input and the last engine. Empty disables the hotkey.
    pub toggle: String,
    /// Switch to the next enabled engine. Empty disables the hotkey.
    pub next: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            toggle: "Control+space".into(),
            next: "Control+Shift+space".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    /// Milliseconds to wait for an engine's answer to a key before passing it through.
    pub key_timeout_ms: u64,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            key_timeout_ms: 200,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Remember the engine of each application, instead of using one engine for all.
    pub remember: bool,
    /// Remember each application's confirmed input mode within an input method.
    /// Only takes effect when `remember` is enabled and the engine supports modes.
    pub remember_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TraySettings {
    /// Show a tray item (StatusNotifierItem) with the current engine.
    pub enabled: bool,
}

impl Default for TraySettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelSettings {
    /// Show the switcher (`omime-panel`) when the user switches input methods with the
    /// hotkeys or the command line.
    pub enabled: bool,
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    /// Show desktop notifications about trouble: an engine that stopped, another input
    /// method on a seat, a mistake in config.toml.
    pub enabled: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    /// Reads a configuration, and the keys in it that this version does not know
    /// (misspelt, or from a newer version), which it ignores.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        crate::from_toml(text)
    }
}

/// A value for [`edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Integer(i64),
    String(String),
    List(Vec<String>),
}

/// The options [`edit`] can set, by dotted name.
pub const KEYS: [&str; 10] = [
    "engines",
    "initial_input_method",
    "hotkeys.toggle",
    "hotkeys.next",
    "engine.key_timeout_ms",
    "apps.remember",
    "apps.remember_mode",
    "tray.enabled",
    "panel.enabled",
    "notifications.enabled",
];

/// Returns `text`, a config.toml, with the option `key` (one of [`KEYS`]) set to
/// `value`. Comments and everything else stay as they are. The result must still be a
/// valid configuration: values of the wrong type, unknown keys and hotkeys that do not
/// parse are errors.
pub fn edit(text: &str, key: &str, value: Value) -> Result<String, String> {
    if !KEYS.contains(&key) {
        return Err(tr!("there is no option `{key}`", key = key));
    }
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let mut item = match value {
        Value::Bool(b) => toml_edit::value(b),
        Value::Integer(i) => toml_edit::value(i),
        Value::String(s) => toml_edit::value(s),
        Value::List(list) => toml_edit::value(
            list.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        ),
    };
    match key.split_once('.') {
        None => doc[key] = item,
        Some((table, name)) => {
            let section = doc
                .entry(table)
                .or_insert(toml_edit::table())
                .as_table_like_mut()
                .ok_or_else(|| {
                    tr!(
                        "configuration section `{table}` must be a table",
                        table = table
                    )
                })?;
            if let Some(previous) = section.get_mut(name) {
                if let (Some(old_value), Some(value)) = (previous.as_value(), item.as_value_mut()) {
                    // Keep whitespace and trailing comments on the replaced value.
                    *value.decor_mut() = old_value.decor().clone();
                }
                *previous = item;
            } else {
                section.insert(name, item);
            }
        }
    }
    let edited = doc.to_string();
    let (config, _) = Config::parse(&edited)?;
    for hotkey in [&config.hotkeys.toggle, &config.hotkeys.next] {
        HotkeySpec::parse(hotkey)?;
    }
    if config.engine.key_timeout_ms == 0 {
        return Err(tr!("`engine.key_timeout_ms` must be at least 1"));
    }
    if let Some(id) = &config.initial_input_method
        && !valid_input_method(id)
    {
        return Err(tr!(
            "`initial_input_method` must be an engine id, engine/method, or direct"
        ));
    }
    Ok(edited)
}

pub fn valid_input_method(id: &str) -> bool {
    let (engine, inner) = id.split_once('/').map_or((id, None), |(e, i)| (e, Some(i)));
    crate::manifest::is_valid_engine_id(engine)
        && inner.is_none_or(|i| !i.is_empty() && !i.chars().any(char::is_control))
        && (engine != "direct" || inner.is_none())
}

/// A hotkey such as `Control+Shift+space`: modifier names and a keysym name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    /// Bits from [`crate::engine::modifiers`].
    pub modifiers: u32,
    /// XKB keysym name, e.g. `space`.
    pub keysym_name: String,
}

impl HotkeySpec {
    /// Parses `Mod+Mod+key`. Returns `Ok(None)` for an empty string (disabled).
    pub fn parse(text: &str) -> Result<Option<Self>, String> {
        use crate::engine::modifiers as m;
        if text.chars().any(char::is_control) {
            return Err(tr!("a hotkey cannot contain control characters"));
        }
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last().ok_or("empty hotkey")?;
        if key.is_empty() {
            return Err(tr!("`{text}` has no key", text = text));
        }
        let mut modifiers = 0;
        for name in mods {
            modifiers |= match name.to_ascii_lowercase().as_str() {
                "shift" => m::SHIFT,
                "control" | "ctrl" => m::CONTROL,
                "alt" | "mod1" => m::ALT,
                "super" | "logo" | "mod4" => m::SUPER,
                other => {
                    return Err(tr!(
                        "unknown modifier `{other}` in `{text}`",
                        other = other,
                        text = text
                    ));
                }
            };
        }
        Ok(Some(Self {
            modifiers,
            keysym_name: (*key).to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::modifiers as m;

    #[test]
    fn defaults_apply_to_missing_sections() {
        let (config, unknown) = Config::parse("engines = [\"echo\"]").unwrap();
        assert_eq!(config.engines.as_deref(), Some(&["echo".to_owned()][..]));
        assert_eq!(config.hotkeys, Hotkeys::default());
        assert_eq!(config.engine.key_timeout_ms, 200);
        assert!(!config.apps.remember);
        assert!(!config.apps.remember_mode);
        assert!(unknown.is_empty());
        assert!(
            Config::parse("[apps]\nremember = true")
                .unwrap()
                .0
                .apps
                .remember
        );
        assert_eq!(Config::parse("").unwrap(), (Config::default(), Vec::new()));
    }

    #[test]
    fn unknown_keys_are_ignored_and_reported() {
        let (config, unknown) =
            Config::parse("bogus = 1\n[hotkeys]\ntogle = \"Super+space\"\n[future]\nx = 1\n")
                .unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(unknown, ["bogus", "future", "hotkeys.togle"]);
        assert!(Config::parse("[apps]\nremember = \"yes\"").is_err());
        assert!(Config::parse("[apps]\nremember_mode = \"yes\"").is_err());
    }

    #[test]
    fn options_are_edited_in_place() {
        let text =
            "# mine\nengines = [\"echo\"]\n\n[hotkeys]\n# the toggle\ntoggle = \"Super+space\"\n";
        let edited = edit(text, "apps.remember", Value::Bool(true)).unwrap();
        assert!(
            edited.starts_with("# mine\nengines = [\"echo\"]"),
            "{edited}"
        );
        assert!(
            edited.contains("# the toggle\ntoggle = \"Super+space\""),
            "{edited}"
        );
        assert!(Config::parse(&edited).unwrap().0.apps.remember);
        let edited = edit(&edited, "apps.remember_mode", Value::Bool(true)).unwrap();
        assert!(Config::parse(&edited).unwrap().0.apps.remember_mode);
        let edited = edit(&edited, "hotkeys.next", Value::String("Super+n".into())).unwrap();
        assert_eq!(Config::parse(&edited).unwrap().0.hotkeys.next, "Super+n");
        let list = Value::List(vec!["fcitx5/shuangpin".into(), "sparkle".into()]);
        let edited = edit(&edited, "engines", list).unwrap();
        assert_eq!(
            Config::parse(&edited).unwrap().0.engines.unwrap(),
            ["fcitx5/shuangpin", "sparkle"]
        );
        let edited = edit("", "engine.key_timeout_ms", Value::Integer(300)).unwrap();
        assert_eq!(Config::parse(&edited).unwrap().0.engine.key_timeout_ms, 300);
    }

    #[test]
    fn edits_must_leave_a_valid_configuration() {
        assert!(edit("", "apps.remeber", Value::Bool(true)).is_err());
        assert!(edit("", "apps.remember", Value::String("yes".into())).is_err());
        assert!(edit("", "apps.remember_mode", Value::String("yes".into())).is_err());
        assert!(edit("", "hotkeys.toggle", Value::String("Hyper+a".into())).is_err());
        assert!(edit("", "engine.key_timeout_ms", Value::Integer(0)).is_err());
        assert!(edit("", "engine.key_timeout_ms", Value::Integer(-5)).is_err());
    }

    #[test]
    fn hotkeys_parse() {
        assert_eq!(
            HotkeySpec::parse("Control+Shift+space").unwrap(),
            Some(HotkeySpec {
                modifiers: m::CONTROL | m::SHIFT,
                keysym_name: "space".into(),
            })
        );
        assert_eq!(
            HotkeySpec::parse("super + F9").unwrap().unwrap().modifiers,
            m::SUPER
        );
        assert_eq!(HotkeySpec::parse("").unwrap(), None);
        assert!(HotkeySpec::parse("Hyper+a").is_err());
        assert!(HotkeySpec::parse("Control+").is_err());
    }
}
