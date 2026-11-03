//! Hotkeys as config.toml writes them (`Control+Shift+space`): made from the keys pressed
//! in the window, and taken apart into the keys shown on keycaps.

use omime_proto::config::HotkeySpec;
use omime_proto::engine::modifiers as m;
use slint::platform::Key;
use xkbcommon::xkb;

/// What a key pressed while recording a hotkey means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A modifier alone: the hotkey is not complete yet.
    Pending,
    /// Escape alone: no new hotkey.
    Cancelled,
    /// The hotkey, as config.toml writes it.
    Hotkey(String),
}

/// Keys that Slint reports with codes of its own, and their keysym names.
const NAMED: [(Key, &str); 23] = [
    (Key::Backspace, "BackSpace"),
    (Key::Tab, "Tab"),
    (Key::Backtab, "ISO_Left_Tab"),
    (Key::Return, "Return"),
    (Key::Escape, "Escape"),
    (Key::Delete, "Delete"),
    (Key::Space, "space"),
    (Key::UpArrow, "Up"),
    (Key::DownArrow, "Down"),
    (Key::LeftArrow, "Left"),
    (Key::RightArrow, "Right"),
    (Key::Insert, "Insert"),
    (Key::Home, "Home"),
    (Key::End, "End"),
    (Key::PageUp, "Prior"),
    (Key::PageDown, "Next"),
    (Key::ScrollLock, "Scroll_Lock"),
    (Key::Pause, "Pause"),
    (Key::SysReq, "Print"),
    (Key::Menu, "Menu"),
    (Key::Stop, "Cancel"),
    (Key::Back, "XF86Back"),
    (Key::CapsLock, "Caps_Lock"),
];

const MODIFIER_KEYS: [Key; 8] = [
    Key::Shift,
    Key::ShiftR,
    Key::Control,
    Key::ControlR,
    Key::Alt,
    Key::AltGr,
    Key::Meta,
    Key::MetaR,
];

/// The hotkey for the key Slint reports as `text`, pressed with these modifiers.
pub fn record(text: &str, control: bool, alt: bool, shift: bool, meta: bool) -> Recorded {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Recorded::Pending;
    };
    if MODIFIER_KEYS.iter().any(|&k| char::from(k) == c) {
        return Recorded::Pending;
    }
    let key = if let Some((_, name)) = NAMED.iter().find(|&&(k, _)| char::from(k) == c) {
        (*name).to_owned()
    } else if let Some(n) = function_key(c) {
        format!("F{n}")
    } else if c.is_control() || ('\u{f700}'..='\u{f8ff}').contains(&c) {
        // A key Slint has a code for that is not worth a hotkey.
        return Recorded::Pending;
    } else {
        // Shift is a modifier of its own: Shift+A is `Shift+a`.
        let c = if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            c
        };
        xkb::keysym_get_name(xkb::utf32_to_keysym(u32::from(c)))
    };
    if key == "Escape" && !(control || alt || shift || meta) {
        return Recorded::Cancelled;
    }
    let mut parts: Vec<&str> = Vec::new();
    for (held, name) in [
        (meta, "Super"),
        (control, "Control"),
        (alt, "Alt"),
        (shift, "Shift"),
    ] {
        if held {
            parts.push(name);
        }
    }
    parts.push(&key);
    Recorded::Hotkey(parts.join("+"))
}

/// The number of a function key in Slint's codes.
fn function_key(c: char) -> Option<u32> {
    let first = u32::from(char::from(Key::F1));
    let n = u32::from(c).checked_sub(first)? + 1;
    (n <= 24).then_some(n)
}

/// The keys of a hotkey for keycaps: `Control+Shift+space` is Ctrl, Shift, Space. A
/// hotkey that is off has none, one omimed cannot read shows as it is written.
pub fn keys(hotkey: &str) -> Vec<String> {
    let spec = match HotkeySpec::parse(hotkey) {
        Ok(Some(spec)) => spec,
        Ok(None) => return Vec::new(),
        Err(_) => return vec![hotkey.to_owned()],
    };
    let mut keys = Vec::new();
    for (bit, name) in [
        (m::SUPER, "Super"),
        (m::CONTROL, "Ctrl"),
        (m::ALT, "Alt"),
        (m::SHIFT, "Shift"),
    ] {
        if spec.modifiers & bit != 0 {
            keys.push(name.to_owned());
        }
    }
    keys.push(key_name(&spec.keysym_name));
    keys
}

/// How a keycap names a key: `Space`, `Enter`, `A`, `!`.
fn key_name(keysym_name: &str) -> String {
    let named = match keysym_name.to_ascii_lowercase().as_str() {
        "space" => Some("Space"),
        "return" | "kp_enter" => Some("Enter"),
        "backspace" => Some("Backspace"),
        "escape" => Some("Esc"),
        "tab" | "iso_left_tab" => Some("Tab"),
        "delete" => Some("Delete"),
        "insert" => Some("Insert"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "prior" | "page_up" => Some("Page Up"),
        "next" | "page_down" => Some("Page Down"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "left" => Some("←"),
        "right" => Some("→"),
        "print" => Some("Print"),
        "menu" => Some("Menu"),
        "caps_lock" => Some("Caps Lock"),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_owned();
    }
    let keysym = xkb::keysym_from_name(keysym_name, xkb::KEYSYM_CASE_INSENSITIVE);
    match char::from_u32(xkb::keysym_to_utf32(keysym)) {
        Some(c) if !c.is_control() && !c.is_whitespace() && c != '\0' => c.to_uppercase().collect(),
        _ => keysym_name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: Key) -> String {
        char::from(k).to_string()
    }

    #[test]
    fn pressed_keys_become_hotkeys_omimed_reads() {
        let space = key(Key::Space);
        assert_eq!(
            record(&space, true, false, true, false),
            Recorded::Hotkey("Control+Shift+space".to_owned())
        );
        assert_eq!(
            record("A", false, false, true, true),
            Recorded::Hotkey("Super+Shift+a".to_owned())
        );
        assert_eq!(
            record("!", true, false, true, false),
            Recorded::Hotkey("Control+Shift+exclam".to_owned())
        );
        assert_eq!(
            record(&key(Key::F5), false, false, false, false),
            Recorded::Hotkey("F5".to_owned())
        );
        assert_eq!(
            record(&key(Key::Return), false, true, false, false),
            Recorded::Hotkey("Alt+Return".to_owned())
        );
        for hotkey in [
            "Control+Shift+space",
            "Super+Shift+a",
            "Control+Shift+exclam",
        ] {
            assert!(HotkeySpec::parse(hotkey).is_ok_and(|s| s.is_some()));
        }
    }

    #[test]
    fn modifiers_alone_wait_and_escape_cancels() {
        assert_eq!(
            record(&key(Key::Control), true, false, false, false),
            Recorded::Pending
        );
        assert_eq!(
            record(&key(Key::Shift), true, false, true, false),
            Recorded::Pending
        );
        assert_eq!(
            record(&key(Key::Meta), false, false, false, true),
            Recorded::Pending
        );
        assert_eq!(record("", false, false, false, false), Recorded::Pending);
        assert_eq!(
            record(&key(Key::Escape), false, false, false, false),
            Recorded::Cancelled
        );
        assert_eq!(
            record(&key(Key::Escape), true, false, false, false),
            Recorded::Hotkey("Control+Escape".to_owned())
        );
    }

    #[test]
    fn hotkeys_show_as_keycaps() {
        assert_eq!(keys("Control+Shift+space"), ["Ctrl", "Shift", "Space"]);
        assert_eq!(keys("Shift+Super+a"), ["Super", "Shift", "A"]);
        assert_eq!(keys("Control+exclam"), ["Ctrl", "!"]);
        assert_eq!(keys("Alt+Return"), ["Alt", "Enter"]);
        assert_eq!(keys("F12"), ["F12"]);
        assert!(keys("").is_empty());
        assert_eq!(keys("Hyper+x"), ["Hyper+x"], "unreadable, shown as written");
    }
}
