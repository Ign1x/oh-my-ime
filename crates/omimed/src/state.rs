//! State that survives restarts (`$XDG_STATE_HOME/oh-my-ime/state.toml`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::core::frameworks::InnerMethod;

const STATE_FILE: &str = "state.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The engine in use when the daemon stopped (absent for direct input).
    pub current: Option<String>,
    /// The engine `toggle` returns to.
    pub last: Option<String>,
    /// The engine last used in each application, by app id (`""` for direct input).
    pub apps: BTreeMap<String, String>,
    /// Confirmed mode by application and full input-method id.
    pub app_modes: BTreeMap<String, BTreeMap<String, String>>,
    /// The input method in use inside each hosted framework, by engine.
    pub inner: BTreeMap<String, String>,
    /// The input methods each hosted framework had when omimed last saw it, so that
    /// they can be chosen before it starts.
    pub methods: BTreeMap<String, Vec<InnerMethod>>,
}

fn path() -> Option<PathBuf> {
    omime_proto::paths::state_dir().map(|d| d.join(STATE_FILE))
}

impl State {
    pub fn load() -> Option<Self> {
        let path = path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(state) => Some(state),
                Err(e) => {
                    warn!("ignoring {}: {e}", path.display());
                    None
                }
            },
            Err(_) => None,
        }
    }

    /// Writes the state atomically (write to a temporary file, then rename).
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text = toml::to_string(self).map_err(std::io::Error::other)?;
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            warn!("cannot save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_toml() {
        let state = State {
            current: Some("sparkle".into()),
            last: None,
            apps: BTreeMap::from([
                ("org.mozilla.firefox".into(), "sparkle".into()),
                ("foot".into(), String::new()),
                ("kitty".into(), "fcitx5/shuangpin".into()),
            ]),
            app_modes: BTreeMap::from([(
                "org.example.Chat".into(),
                BTreeMap::from([("rime/luna_pinyin".into(), "native".into())]),
            )]),
            inner: BTreeMap::from([("fcitx5".into(), "shuangpin".into())]),
            methods: BTreeMap::from([(
                "fcitx5".into(),
                vec![InnerMethod {
                    id: "shuangpin".into(),
                    name: "双拼".into(),
                    icon: "fcitx-shuangpin".into(),
                    label: "双".into(),
                    language: "zh_CN".into(),
                }],
            )]),
        };
        let text = toml::to_string(&state).unwrap();
        assert_eq!(toml::from_str::<State>(&text).unwrap(), state);
        assert_eq!(toml::from_str::<State>("").unwrap(), State::default());
    }

    #[test]
    fn pre_mode_state_files_still_restore_application_choices() {
        let state: State =
            toml::from_str("current = 'rime'\n[apps]\nchat = 'rime/luna_pinyin'\n").unwrap();
        assert_eq!(state.apps["chat"], "rime/luna_pinyin");
        assert!(state.app_modes.is_empty());
    }
}
