//! Keyboard bookkeeping for the Wayland frontend.
//!
//! The compositor hands the keyboard grab a keymap, modifier state and raw evdev key
//! codes. This module decodes keys with xkbcommon, remembers which keys reached the
//! application through our virtual keyboard, and prepares keymaps for upload.

use std::collections::BTreeSet;
use std::io;
use std::os::fd::OwnedFd;

use rustix::fs::{MemfdFlags, SealFlags};
use xkbcommon::xkb;

/// Offset between evdev key codes (used on the wire) and XKB key codes.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once engines receive keys (M2)")
)]
const EVDEV_XKB_OFFSET: u32 = 8;

/// Modifier and layout state as sent by `zwp_input_method_keyboard_grab_v2.modifiers`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModifierState {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}

/// Key repeat parameters from `zwp_input_method_keyboard_grab_v2.repeat_info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatInfo {
    /// Repeats per second; 0 disables repeat.
    pub rate: i32,
    /// Delay before the first repeat, in milliseconds.
    pub delay: i32,
}

impl Default for RepeatInfo {
    fn default() -> Self {
        Self {
            rate: 25,
            delay: 600,
        }
    }
}

/// A key decoded with the current keymap and modifier state.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once engines receive keys (M2)")
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedKey {
    pub keysym: u32,
    pub utf8: String,
    pub repeats: bool,
}

/// Errors while handling keymaps.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    #[error("failed to read the keymap: {0}")]
    Read(#[from] io::Error),
    #[error("xkbcommon could not compile the keymap")]
    Compile,
}

struct LoadedKeymap {
    text: String,
    state: xkb::State,
}

/// Keymap, modifier and repeat state of one keyboard grab.
pub struct Keyboard {
    context: xkb::Context,
    keymap: Option<LoadedKeymap>,
    modifiers: ModifierState,
    repeat: RepeatInfo,
}

impl std::fmt::Debug for Keyboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyboard")
            .field("has_keymap", &self.keymap.is_some())
            .field("modifiers", &self.modifiers)
            .field("repeat", &self.repeat)
            .finish()
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            keymap: None,
            modifiers: ModifierState::default(),
            repeat: RepeatInfo::default(),
        }
    }

    /// Installs a new keymap. Returns `false` if it is identical to the current one.
    pub fn set_keymap(&mut self, text: String) -> Result<bool, KeymapError> {
        if self.keymap_text() == Some(text.as_str()) {
            return Ok(false);
        }
        let keymap = xkb::Keymap::new_from_string(
            &self.context,
            text.clone(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or(KeymapError::Compile)?;
        let mut state = xkb::State::new(&keymap);
        apply_modifiers(&mut state, self.modifiers);
        self.keymap = Some(LoadedKeymap { text, state });
        Ok(true)
    }

    /// The text of the current keymap, if any.
    pub fn keymap_text(&self) -> Option<&str> {
        self.keymap.as_ref().map(|k| k.text.as_str())
    }

    pub fn set_modifiers(&mut self, modifiers: ModifierState) {
        self.modifiers = modifiers;
        if let Some(keymap) = &mut self.keymap {
            apply_modifiers(&mut keymap.state, modifiers);
        }
    }

    pub fn modifiers(&self) -> ModifierState {
        self.modifiers
    }

    pub fn set_repeat_info(&mut self, repeat: RepeatInfo) {
        self.repeat = repeat;
    }

    #[expect(dead_code, reason = "used by key repeat for engines (M2)")]
    pub fn repeat_info(&self) -> RepeatInfo {
        self.repeat
    }

    /// Decodes an evdev key code with the current keymap and modifiers.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used once engines receive keys (M2)")
    )]
    pub fn decode(&self, evdev_key: u32) -> Option<DecodedKey> {
        let keymap = self.keymap.as_ref()?;
        let code = xkb::Keycode::new(evdev_key + EVDEV_XKB_OFFSET);
        Some(DecodedKey {
            keysym: keymap.state.key_get_one_sym(code).raw(),
            utf8: keymap.state.key_get_utf8(code),
            repeats: keymap.state.get_keymap().key_repeats(code),
        })
    }
}

fn apply_modifiers(state: &mut xkb::State, m: ModifierState) {
    state.update_mask(m.depressed, m.latched, m.locked, 0, 0, m.group);
}

/// Reads a keymap sent by the compositor (`keymap` event) into a string.
pub fn read_keymap(fd: &OwnedFd, size: u32) -> io::Result<String> {
    let mut buf = vec![0u8; size as usize];
    let mut filled = 0;
    while filled < buf.len() {
        let n = rustix::io::pread(fd, &mut buf[filled..], filled as u64)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes a keymap into a sealed memfd suitable for `zwp_virtual_keyboard_v1.keymap`.
///
/// Returns the file descriptor and the size including the terminating NUL byte.
pub fn keymap_memfd(text: &str) -> io::Result<(OwnedFd, u32)> {
    let fd = rustix::fs::memfd_create(
        "omime-keymap",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    let mut written = 0;
    while written < bytes.len() {
        written += rustix::io::write(&fd, &bytes[written..])?;
    }
    rustix::fs::fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    let size = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "keymap too large"))?;
    Ok((fd, size))
}

/// What to do with a key release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseAction {
    /// Send the release to the application.
    Forward,
    /// The press was consumed by an engine; swallow the release too.
    Swallow,
}

/// Remembers how each held key's press was handled so its release can be routed the
/// same way.
#[derive(Debug, Default)]
pub struct KeyTracker {
    forwarded: BTreeSet<u32>,
    consumed: BTreeSet<u32>,
}

impl KeyTracker {
    /// The press of `key` was sent to the application.
    pub fn press_forwarded(&mut self, key: u32) {
        self.consumed.remove(&key);
        self.forwarded.insert(key);
    }

    /// The press of `key` was consumed by an engine.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used once engines receive keys (M2)")
    )]
    pub fn press_consumed(&mut self, key: u32) {
        self.forwarded.remove(&key);
        self.consumed.insert(key);
    }

    /// Decides how to route the release of `key`.
    ///
    /// Releases of keys we have never seen pressed are forwarded: they were pressed
    /// before the grab started, and swallowing them would leave the application with a
    /// stuck, auto-repeating key.
    pub fn release(&mut self, key: u32) -> ReleaseAction {
        self.forwarded.remove(&key);
        if self.consumed.remove(&key) {
            ReleaseAction::Swallow
        } else {
            ReleaseAction::Forward
        }
    }

    /// Whether the application currently holds a press of `key` from us.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by key repeat for engines (M2)")
    )]
    pub fn is_forwarded(&self, key: u32) -> bool {
        self.forwarded.contains(&key)
    }

    /// Forgets all held keys and returns those whose release the application still
    /// expects.
    pub fn take_forwarded(&mut self) -> Vec<u32> {
        self.consumed.clear();
        std::mem::take(&mut self.forwarded).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us_keymap() -> String {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "pc105",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the us keymap must compile");
        keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }

    // evdev codes from linux/input-event-codes.h
    const KEY_A: u32 = 30;
    const KEY_1: u32 = 2;
    const KEY_LEFTSHIFT: u32 = 42;

    #[test]
    fn decodes_keys_with_modifiers() {
        let mut keyboard = Keyboard::new();
        assert!(keyboard.decode(KEY_A).is_none());
        assert!(keyboard.set_keymap(us_keymap()).unwrap());
        assert!(!keyboard.set_keymap(us_keymap()).unwrap(), "same keymap");

        let a = keyboard.decode(KEY_A).unwrap();
        assert_eq!(a.utf8, "a");
        assert_eq!(a.keysym, xkb::keysyms::KEY_a);
        assert!(a.repeats);

        // Shift is modifier bit 0 in every xkb keymap.
        keyboard.set_modifiers(ModifierState {
            depressed: 1,
            ..Default::default()
        });
        assert_eq!(keyboard.decode(KEY_A).unwrap().utf8, "A");
        assert_eq!(keyboard.decode(KEY_1).unwrap().utf8, "!");
        assert!(!keyboard.decode(KEY_LEFTSHIFT).unwrap().repeats);
    }

    #[test]
    fn keymap_survives_memfd_round_trip() {
        let text = us_keymap();
        let (fd, size) = keymap_memfd(&text).unwrap();
        assert_eq!(size as usize, text.len() + 1);
        assert_eq!(read_keymap(&fd, size).unwrap(), text);
    }

    #[test]
    fn releases_follow_their_presses() {
        let mut keys = KeyTracker::default();
        keys.press_forwarded(KEY_A);
        keys.press_consumed(KEY_1);
        assert!(keys.is_forwarded(KEY_A));
        assert_eq!(keys.release(KEY_A), ReleaseAction::Forward);
        assert_eq!(keys.release(KEY_1), ReleaseAction::Swallow);
        // Pressed before the grab existed: the application must see the release.
        assert_eq!(keys.release(KEY_LEFTSHIFT), ReleaseAction::Forward);
    }

    #[test]
    fn take_forwarded_returns_held_keys_only() {
        let mut keys = KeyTracker::default();
        keys.press_forwarded(KEY_A);
        keys.press_forwarded(KEY_LEFTSHIFT);
        keys.press_consumed(KEY_1);
        assert_eq!(keys.take_forwarded(), vec![KEY_A, KEY_LEFTSHIFT]);
        assert!(keys.take_forwarded().is_empty());
        assert_eq!(keys.release(KEY_1), ReleaseAction::Forward);
    }
}
