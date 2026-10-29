//! Wire types of the engine protocol (`io.github.ign1x.OhMyIme.Engine1` and
//! `io.github.ign1x.OhMyIme.EngineHost1`).
//!
//! All offsets and lengths into text are in UTF-8 **bytes**, exactly like
//! `zwp_text_input_v3`. Engines know the strings involved and can convert; the daemon
//! never has to guess.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::{OwnedVariant, Type, Variant};

use crate::{ContentHints, ContentPurpose, ContentType};

/// Identifies one focused text field for as long as it has focus.
pub type ContextId = u64;

/// Modifier bits used in [`KeyEvent::modifiers`]. The values match the X11/XKB core
/// modifier masks that most input method code already knows.
pub mod modifiers {
    pub const SHIFT: u32 = 1 << 0;
    pub const CAPS_LOCK: u32 = 1 << 1;
    pub const CONTROL: u32 = 1 << 2;
    pub const ALT: u32 = 1 << 3;
    pub const NUM_LOCK: u32 = 1 << 4;
    pub const SUPER: u32 = 1 << 6;
}

/// A key press or release delivered to an engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct KeyEvent {
    /// The XKB keysym under the current layout and modifiers.
    pub keysym: u32,
    /// The evdev key code (XKB key code minus 8).
    pub keycode: u32,
    /// Active modifiers, see [`modifiers`].
    pub modifiers: u32,
    /// Timestamp in milliseconds with an undefined base.
    pub time: u32,
    /// Text the key would produce without an input method (may be empty).
    pub text: String,
    /// `true` for a release, `false` for a press.
    pub released: bool,
    /// `true` for a press synthesised by key repeat.
    pub repeat: bool,
}

impl KeyEvent {
    /// Returns whether all `mask` modifiers are active.
    pub fn has(&self, mask: u32) -> bool {
        self.modifiers & mask == mask
    }
}

/// Text being composed, shown inline in the focused field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Preedit {
    pub text: String,
    /// Cursor start, in bytes into `text`; `-1` hides the cursor.
    pub cursor_begin: i32,
    /// Cursor end, in bytes into `text`; `-1` hides the cursor.
    pub cursor_end: i32,
}

impl Preedit {
    /// A preedit with the cursor at its end.
    pub fn with_cursor_at_end(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = i32::try_from(text.len()).unwrap_or(i32::MAX);
        Self {
            text,
            cursor_begin: end,
            cursor_end: end,
        }
    }
}

/// Something an engine asks the daemon to do to the focused text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Insert text at the cursor (replacing the preedit).
    Commit(String),
    /// Show (or, with empty text, clear) the preedit.
    Preedit(Preedit),
    /// Delete text around the cursor; lengths in bytes.
    DeleteSurrounding { before: u32, after: u32 },
    /// Send a key to the application.
    ForwardKey { keycode: u32, pressed: bool },
}

const ACTION_COMMIT: u32 = 0;
const ACTION_PREEDIT: u32 = 1;
const ACTION_DELETE_SURROUNDING: u32 = 2;
const ACTION_FORWARD_KEY: u32 = 3;

/// An action as sent over D-Bus: `(kind, payload)`.
pub type WireAction = (u32, OwnedVariant);

/// Error for values that do not have the expected D-Bus type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

fn owned(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("values without file descriptors can be owned")
}

impl Action {
    pub fn to_wire(&self) -> WireAction {
        match self {
            Self::Commit(text) => (ACTION_COMMIT, owned(Variant::from(text.as_str()))),
            Self::Preedit(p) => (
                ACTION_PREEDIT,
                owned(Variant::from((
                    p.text.as_str(),
                    p.cursor_begin,
                    p.cursor_end,
                ))),
            ),
            Self::DeleteSurrounding { before, after } => (
                ACTION_DELETE_SURROUNDING,
                owned(Variant::from((*before, *after))),
            ),
            Self::ForwardKey { keycode, pressed } => (
                ACTION_FORWARD_KEY,
                owned(Variant::from((*keycode, *pressed))),
            ),
        }
    }

    pub fn from_wire((kind, value): &WireAction) -> Result<Self, WireError> {
        let bad = |what: &str| WireError(format!("malformed {what} action"));
        Ok(match *kind {
            ACTION_COMMIT => Self::Commit(
                String::try_from(value.try_clone().map_err(|_| bad("commit"))?)
                    .map_err(|_| bad("commit"))?,
            ),
            ACTION_PREEDIT => {
                let (text, cursor_begin, cursor_end) =
                    <(String, i32, i32)>::try_from(value.try_clone().map_err(|_| bad("preedit"))?)
                        .map_err(|_| bad("preedit"))?;
                Self::Preedit(Preedit {
                    text,
                    cursor_begin,
                    cursor_end,
                })
            }
            ACTION_DELETE_SURROUNDING => {
                let (before, after) = <(u32, u32)>::try_from(
                    value.try_clone().map_err(|_| bad("delete-surrounding"))?,
                )
                .map_err(|_| bad("delete-surrounding"))?;
                Self::DeleteSurrounding { before, after }
            }
            ACTION_FORWARD_KEY => {
                let (keycode, pressed) =
                    <(u32, bool)>::try_from(value.try_clone().map_err(|_| bad("forward-key"))?)
                        .map_err(|_| bad("forward-key"))?;
                Self::ForwardKey { keycode, pressed }
            }
            other => return Err(WireError(format!("unknown action kind {other}"))),
        })
    }
}

/// An `a{sv}` dictionary.
pub type Dict = HashMap<String, OwnedVariant>;

/// Rectangle `(x, y, width, height)`.
pub type Rect = (i32, i32, i32, i32);

/// What an engine knows about a focused text field.
///
/// Sent in full with `Activate` and as changes with `UpdateContext`; keys that are
/// absent from an update keep their value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextInfo {
    /// Application identifier (Wayland app id or X11 class), when known.
    pub app_id: Option<String>,
    pub content_type: ContentType,
    /// Text around the cursor; `cursor` and `anchor` are byte offsets into it.
    pub surrounding: Option<(String, u32, u32)>,
    /// The text cursor relative to the engine's popup window, when it is shown.
    pub cursor_rect: Option<Rect>,
}

mod key {
    pub const APP_ID: &str = "app-id";
    pub const CONTENT_PURPOSE: &str = "content-purpose";
    pub const CONTENT_HINTS: &str = "content-hints";
    pub const SURROUNDING_TEXT: &str = "surrounding-text";
    pub const SURROUNDING_CURSOR: &str = "surrounding-cursor";
    pub const SURROUNDING_ANCHOR: &str = "surrounding-anchor";
    pub const CURSOR_RECT: &str = "cursor-rect";
}

impl ContextInfo {
    /// Encodes every field that is set.
    pub fn to_dict(&self) -> Dict {
        let mut dict = Dict::new();
        if let Some(app_id) = &self.app_id {
            dict.insert(key::APP_ID.into(), owned(Variant::from(app_id.as_str())));
        }
        dict.insert(
            key::CONTENT_PURPOSE.into(),
            owned(Variant::from(self.content_type.purpose.to_raw())),
        );
        dict.insert(
            key::CONTENT_HINTS.into(),
            owned(Variant::from(self.content_type.hints.0)),
        );
        if let Some((text, cursor, anchor)) = &self.surrounding {
            dict.insert(
                key::SURROUNDING_TEXT.into(),
                owned(Variant::from(text.as_str())),
            );
            dict.insert(
                key::SURROUNDING_CURSOR.into(),
                owned(Variant::from(*cursor)),
            );
            dict.insert(
                key::SURROUNDING_ANCHOR.into(),
                owned(Variant::from(*anchor)),
            );
        }
        if let Some(rect) = self.cursor_rect {
            dict.insert(key::CURSOR_RECT.into(), owned(Variant::from(rect)));
        }
        dict
    }

    /// Applies the keys present in `dict`; unknown keys are ignored.
    pub fn apply(&mut self, dict: &Dict) {
        fn get<T: TryFrom<OwnedVariant>>(dict: &Dict, key: &str) -> Option<T> {
            dict.get(key)
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| T::try_from(v).ok())
        }
        if let Some(app_id) = get::<String>(dict, key::APP_ID) {
            self.app_id = Some(app_id);
        }
        if let Some(purpose) = get::<u32>(dict, key::CONTENT_PURPOSE) {
            self.content_type.purpose = ContentPurpose::from_raw(purpose);
        }
        if let Some(hints) = get::<u32>(dict, key::CONTENT_HINTS) {
            self.content_type.hints = ContentHints(hints);
        }
        if let Some(text) = get::<String>(dict, key::SURROUNDING_TEXT) {
            let cursor = get::<u32>(dict, key::SURROUNDING_CURSOR).unwrap_or(0);
            let anchor = get::<u32>(dict, key::SURROUNDING_ANCHOR).unwrap_or(cursor);
            self.surrounding = Some((text, cursor, anchor));
        }
        if let Some(rect) = get::<Rect>(dict, key::CURSOR_RECT) {
            self.cursor_rect = Some(rect);
        }
    }

    /// Decodes a full context description.
    pub fn from_dict(dict: &Dict) -> Self {
        let mut info = Self::default();
        info.apply(dict);
        info
    }

    /// Encodes only the fields of `self` that differ from `old`.
    pub fn diff(&self, old: &Self) -> Dict {
        let mut dict = self.to_dict();
        if self.app_id == old.app_id {
            dict.remove(key::APP_ID);
        }
        if self.content_type == old.content_type {
            dict.remove(key::CONTENT_PURPOSE);
            dict.remove(key::CONTENT_HINTS);
        }
        if self.surrounding == old.surrounding {
            dict.remove(key::SURROUNDING_TEXT);
            dict.remove(key::SURROUNDING_CURSOR);
            dict.remove(key::SURROUNDING_ANCHOR);
        }
        if self.cursor_rect == old.cursor_rect {
            dict.remove(key::CURSOR_RECT);
        }
        dict
    }
}

/// Status an engine shows in panels and the CLI (`SetStatus`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Short label, e.g. "中" or "A".
    pub label: Option<String>,
    /// Icon name from the icon theme.
    pub icon: Option<String>,
    /// Longer description.
    pub tooltip: Option<String>,
}

impl Status {
    pub fn to_dict(&self) -> Dict {
        let mut dict = Dict::new();
        for (key, value) in [
            ("label", &self.label),
            ("icon", &self.icon),
            ("tooltip", &self.tooltip),
        ] {
            if let Some(value) = value {
                dict.insert(key.into(), owned(Variant::from(value.as_str())));
            }
        }
        dict
    }

    pub fn from_dict(dict: &Dict) -> Self {
        let get = |key: &str| {
            dict.get(key)
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| String::try_from(v).ok())
        };
        Self {
            label: get("label"),
            icon: get("icon"),
            tooltip: get("tooltip"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_round_trip() {
        let actions = [
            Action::Commit("你好".into()),
            Action::Preedit(Preedit::with_cursor_at_end("ni hao")),
            Action::DeleteSurrounding {
                before: 3,
                after: 0,
            },
            Action::ForwardKey {
                keycode: 28,
                pressed: true,
            },
        ];
        for action in actions {
            assert_eq!(Action::from_wire(&action.to_wire()).unwrap(), action);
        }
        assert!(Action::from_wire(&(99, owned(Variant::from(1u32)))).is_err());
        assert!(Action::from_wire(&(ACTION_COMMIT, owned(Variant::from(1u32)))).is_err());
    }

    #[test]
    fn preedit_cursor_is_in_bytes() {
        let p = Preedit::with_cursor_at_end("中文");
        assert_eq!((p.cursor_begin, p.cursor_end), (6, 6));
    }

    #[test]
    fn context_info_round_trips_and_diffs() {
        let info = ContextInfo {
            app_id: Some("org.example.Editor".into()),
            content_type: ContentType {
                purpose: ContentPurpose::Email,
                hints: ContentHints::COMPLETION,
            },
            surrounding: Some(("hello".into(), 5, 5)),
            cursor_rect: Some((0, -13, 6, 13)),
        };
        assert_eq!(ContextInfo::from_dict(&info.to_dict()), info);

        let mut moved = info.clone();
        moved.cursor_rect = Some((10, -13, 6, 13));
        let diff = moved.diff(&info);
        assert_eq!(diff.len(), 1);
        let mut applied = info.clone();
        applied.apply(&diff);
        assert_eq!(applied, moved);
    }

    #[test]
    fn status_round_trips() {
        let status = Status {
            label: Some("中".into()),
            icon: None,
            tooltip: Some("Chinese".into()),
        };
        assert_eq!(Status::from_dict(&status.to_dict()), status);
    }
}
