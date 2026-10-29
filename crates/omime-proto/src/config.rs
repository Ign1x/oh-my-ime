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
//! ```

use serde::{Deserialize, Serialize};

/// File name of the configuration below `$XDG_CONFIG_HOME/oh-my-ime`.
pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Enabled engine ids in switching order; `None` enables everything installed.
    pub engines: Option<Vec<String>>,
    pub hotkeys: Hotkeys,
    pub engine: EngineSettings,
    pub apps: AppSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// Remember the engine of each application, instead of using one engine for all.
    pub remember: bool,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
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
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last().ok_or("empty hotkey")?;
        if key.is_empty() {
            return Err(format!("`{text}` has no key"));
        }
        let mut modifiers = 0;
        for name in mods {
            modifiers |= match name.to_ascii_lowercase().as_str() {
                "shift" => m::SHIFT,
                "control" | "ctrl" => m::CONTROL,
                "alt" | "mod1" => m::ALT,
                "super" | "logo" | "mod4" => m::SUPER,
                other => return Err(format!("unknown modifier `{other}` in `{text}`")),
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
        let config = Config::parse("engines = [\"echo\"]").unwrap();
        assert_eq!(config.engines.as_deref(), Some(&["echo".to_owned()][..]));
        assert_eq!(config.hotkeys, Hotkeys::default());
        assert_eq!(config.engine.key_timeout_ms, 200);
        assert!(!config.apps.remember);
        assert!(
            Config::parse("[apps]\nremember = true")
                .unwrap()
                .apps
                .remember
        );
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert!(Config::parse("bogus = 1").is_err());
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
