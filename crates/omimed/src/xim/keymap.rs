//! Keysyms of X11 key codes, from the X server's core keyboard mapping.
//!
//! XIM hands over raw key events (a key code and a modifier state); which keysym a key
//! means follows the rules of the X protocol (section 5, "Keyboards"): the first two
//! keysyms of a key are its unshifted and shifted symbols, Lock is Caps Lock for
//! letters, and Num Lock picks the second symbol of keypad keys.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use xkbcommon::xkb;

/// X11 modifier bits.
const SHIFT: u16 = 1 << 0;
const LOCK: u16 = 1 << 1;
const NUM_LOCK: u16 = 1 << 4;

/// The keyboard mapping of an X server.
#[derive(Debug, Default)]
pub struct Keymap {
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Keymap {
    /// Reads the mapping of `conn`'s server.
    pub fn load(conn: &impl Connection) -> anyhow::Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1))?
            .reply()?;
        Ok(Self {
            min_keycode: min,
            keysyms_per_keycode: reply.keysyms_per_keycode,
            keysyms: reply.keysyms,
        })
    }

    /// The keysym `keycode` produces with the modifiers in `state`, or 0.
    pub fn keysym(&self, keycode: u8, state: u16) -> u32 {
        let per = usize::from(self.keysyms_per_keycode);
        let Some(index) = keycode.checked_sub(self.min_keycode) else {
            return 0;
        };
        let start = usize::from(index) * per;
        let Some(syms) = self.keysyms.get(start..start + per.min(2)) else {
            return 0;
        };
        let first = syms.first().copied().unwrap_or(0);
        let second = syms.get(1).copied().unwrap_or(0);
        // A key with one symbol that is a letter has both cases.
        let (lower, upper) = if second == 0 {
            (to_lower(first), to_upper(first))
        } else {
            (first, second)
        };
        let shift = state & SHIFT != 0;
        if state & NUM_LOCK != 0 && is_keypad(upper) {
            return if shift { lower } else { upper };
        }
        let caps = state & LOCK != 0;
        let keysym = if shift { upper } else { lower };
        if caps { to_upper(keysym) } else { keysym }
    }
}

fn is_keypad(keysym: u32) -> bool {
    // XK_KP_Space .. XK_KP_Equal
    (0xff80..=0xffbd).contains(&keysym)
}

fn to_upper(keysym: u32) -> u32 {
    change_case(keysym, true)
}

fn to_lower(keysym: u32) -> u32 {
    change_case(keysym, false)
}

/// The keysym of the other case of the character `keysym` types, if it has one.
fn change_case(keysym: u32, upper: bool) -> u32 {
    let text = xkb::keysym_to_utf8(xkb::Keysym::new(keysym));
    let mut chars = text.trim_end_matches('\0').chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return keysym;
    };
    let changed: Vec<char> = if upper {
        c.to_uppercase().collect()
    } else {
        c.to_lowercase().collect()
    };
    match changed.as_slice() {
        [other] if *other != c => xkb::utf32_to_keysym(u32::from(*other)).raw(),
        _ => keysym,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key codes 8..: `a` (one symbol), `1`/`!`, KP_End/KP_1.
    fn keymap() -> Keymap {
        Keymap {
            min_keycode: 8,
            keysyms_per_keycode: 2,
            keysyms: vec![0x61, 0, 0x31, 0x21, 0xff9c, 0xffb1],
        }
    }

    #[test]
    fn shift_and_caps_lock_pick_symbols() {
        let keymap = keymap();
        assert_eq!(keymap.keysym(8, 0), 0x61);
        assert_eq!(keymap.keysym(8, SHIFT), 0x41);
        assert_eq!(keymap.keysym(8, LOCK), 0x41);
        assert_eq!(keymap.keysym(9, 0), 0x31);
        assert_eq!(keymap.keysym(9, SHIFT), 0x21);
        // Caps Lock does not shift digits.
        assert_eq!(keymap.keysym(9, LOCK), 0x31);
    }

    #[test]
    fn num_lock_picks_keypad_digits() {
        let keymap = keymap();
        assert_eq!(keymap.keysym(10, 0), 0xff9c);
        assert_eq!(keymap.keysym(10, NUM_LOCK), 0xffb1);
        assert_eq!(keymap.keysym(10, NUM_LOCK | SHIFT), 0xff9c);
    }

    #[test]
    fn unknown_key_codes_have_no_symbol() {
        assert_eq!(keymap().keysym(7, 0), 0);
        assert_eq!(keymap().keysym(200, 0), 0);
        assert_eq!(Keymap::default().keysym(8, 0), 0);
    }
}
