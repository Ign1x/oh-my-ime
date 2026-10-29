//! One seat's input method session.

use std::rc::Rc;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};
use wl_proxy::object::{Object, ObjectCoreApi};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::ZwpInputMethodV2;
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wl_proxy::protocols::wayland::wl_seat::WlSeat;

use super::{EventQueue, GrabEvent, GrabEvents, ImEvent, ImEvents, SeatId};
use crate::ime_state::ImState;
use crate::keyboard::{self, KeyTracker, Keyboard, ReleaseAction};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const KEY_STATE_RELEASED: u32 = 0;
const KEY_STATE_PRESSED: u32 = 1;
const KEY_STATE_REPEATED: u32 = 2;

const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// The preedit the daemon currently shows in the focused text field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    pub cursor_begin: i32,
    pub cursor_end: i32,
}

/// The input method, keyboard grab and virtual keyboard of one seat.
pub struct SeatSession {
    id: SeatId,
    name: String,
    seat: Rc<WlSeat>,
    queue: EventQueue,

    im: Option<Rc<ZwpInputMethodV2>>,
    im_generation: u32,
    ime: ImState,
    /// When to request the input method again after `unavailable`.
    retry: Option<(Instant, Duration)>,

    grab: Option<Rc<ZwpInputMethodKeyboardGrabV2>>,
    grab_generation: u32,
    keyboard: Keyboard,
    keys: KeyTracker,

    vk: Rc<ZwpVirtualKeyboardV1>,
    /// The keymap last uploaded to the virtual keyboard. Nothing may be sent to the
    /// virtual keyboard before a keymap: that is a fatal protocol error.
    vk_keymap: Option<String>,

    preedit: Preedit,
}

impl std::fmt::Debug for SeatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeatSession")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("active", &self.ime.current().active)
            .field("grabbed", &self.grab.is_some())
            .finish_non_exhaustive()
    }
}

impl SeatSession {
    pub fn new(
        id: SeatId,
        seat: Rc<WlSeat>,
        im_manager: &Rc<ZwpInputMethodManagerV2>,
        vk_manager: &Rc<ZwpVirtualKeyboardManagerV1>,
        queue: EventQueue,
    ) -> Self {
        let vk = vk_manager.new_send_create_virtual_keyboard(&seat);
        vk.set_forward_to_client(false);
        let mut session = Self {
            id,
            name: format!("seat#{id}"),
            seat,
            queue,
            im: None,
            im_generation: 0,
            ime: ImState::default(),
            retry: None,
            grab: None,
            grab_generation: 0,
            keyboard: Keyboard::new(),
            keys: KeyTracker::default(),
            vk,
            vk_keymap: None,
            preedit: Preedit::default(),
        };
        session.request_input_method(im_manager);
        session
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// The input method object, if we currently own one.
    pub fn input_method(&self) -> Option<&Rc<ZwpInputMethodV2>> {
        self.im.as_ref()
    }

    /// Whether a text input is focused on this seat.
    pub fn is_active(&self) -> bool {
        self.im.is_some() && self.ime.current().active
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.retry.map(|(at, _)| at)
    }

    pub fn on_timer(&mut self, now: Instant, im_manager: &Rc<ZwpInputMethodManagerV2>) {
        if self.im.is_none() && self.retry.is_some_and(|(at, _)| at <= now) {
            info!(seat = self.name, "trying to become the input method again");
            self.request_input_method(im_manager);
        }
    }

    fn request_input_method(&mut self, im_manager: &Rc<ZwpInputMethodManagerV2>) {
        self.im_generation = self.im_generation.wrapping_add(1);
        self.ime.reset();
        let im = im_manager.new_send_get_input_method(&self.seat);
        im.set_forward_to_client(false);
        im.set_handler(ImEvents {
            seat: self.id,
            generation: self.im_generation,
            queue: self.queue.clone(),
        });
        self.im = Some(im);
    }

    pub fn handle_im(&mut self, generation: u32, event: ImEvent) {
        if generation != self.im_generation || self.im.is_none() {
            return;
        }
        match event {
            ImEvent::Activate => self.ime.activate(),
            ImEvent::Deactivate => self.ime.deactivate(),
            ImEvent::SurroundingText {
                text,
                cursor,
                anchor,
            } => self.ime.surrounding_text(text, cursor, anchor),
            ImEvent::TextChangeCause(cause) => self.ime.text_change_cause(cause),
            ImEvent::ContentType { hints, purpose } => self.ime.content_type(hints, purpose),
            ImEvent::Done => self.on_done(),
            ImEvent::Unavailable => self.on_unavailable(),
        }
    }

    fn on_unavailable(&mut self) {
        self.release_grab();
        if let Some(im) = self.im.take() {
            im.unset_handler();
            im.send_destroy();
        }
        let backoff = self
            .retry
            .map_or(RETRY_MIN, |(_, b)| (b * 2).min(RETRY_MAX));
        warn!(
            seat = self.name,
            retry_in = ?backoff,
            "another input method is active on this seat (is fcitx5 or ibus running?)"
        );
        self.retry = Some((Instant::now() + backoff, backoff));
    }

    fn on_done(&mut self) {
        let applied = self.ime.done();
        if self.retry.take().is_some() {
            info!(seat = self.name, "became the input method");
        }
        let current = self.ime.current();
        let sensitive = current.content_type.is_sensitive();
        if applied.activated {
            // Activation resets the text input; whatever we showed is gone.
            self.preedit = Preedit::default();
        }
        if applied.is_active != applied.was_active || applied.activated {
            debug!(
                seat = self.name,
                active = applied.is_active,
                sensitive,
                "text input focus changed"
            );
        }

        let want_grab = applied.is_active && !sensitive;
        if want_grab && self.grab.is_none() {
            self.take_grab();
        } else if !want_grab && self.grab.is_some() {
            self.release_grab();
        }

        // Hyprland only sends text-input-v3 `done` after our commit, and the text
        // input clears its preedit on every `done`. So always commit, and always
        // re-send the preedit we want to keep.
        self.commit(applied.serial);
    }

    fn commit(&self, serial: u32) {
        let Some(im) = &self.im else { return };
        if self.ime.current().active && !self.preedit.text.is_empty() {
            im.send_set_preedit_string(
                &self.preedit.text,
                self.preedit.cursor_begin,
                self.preedit.cursor_end,
            );
        }
        im.send_commit(serial);
    }

    fn take_grab(&mut self) {
        let Some(im) = &self.im else { return };
        self.grab_generation = self.grab_generation.wrapping_add(1);
        let grab = im.new_send_grab_keyboard();
        grab.set_forward_to_client(false);
        grab.set_handler(GrabEvents {
            seat: self.id,
            generation: self.grab_generation,
            queue: self.queue.clone(),
        });
        self.grab = Some(grab);
        debug!(seat = self.name, "keyboard grabbed");
    }

    /// Releases the grab. Keys the application still sees as pressed are released on
    /// the virtual keyboard first; afterwards their real releases would bypass us.
    fn release_grab(&mut self) {
        let Some(grab) = self.grab.take() else { return };
        let time = now_ms();
        for key in self.keys.take_forwarded() {
            self.vk.send_key(time, key, KEY_STATE_RELEASED);
        }
        grab.unset_handler();
        grab.send_release();
        debug!(seat = self.name, "keyboard released");
    }

    pub fn handle_grab(&mut self, generation: u32, event: GrabEvent) {
        if generation != self.grab_generation || self.grab.is_none() {
            return;
        }
        match event {
            GrabEvent::Keymap { format, fd, size } => {
                if format != KEYMAP_FORMAT_XKB_V1 {
                    warn!(
                        seat = self.name,
                        format, "ignoring keymap in unknown format"
                    );
                    return;
                }
                let loaded = keyboard::read_keymap(&fd, size)
                    .map_err(keyboard::KeymapError::from)
                    .and_then(|text| self.keyboard.set_keymap(text));
                match loaded {
                    Ok(true) => debug!(seat = self.name, "new keymap"),
                    Ok(false) => {}
                    Err(e) => error!(seat = self.name, "cannot use the keymap: {e}"),
                }
            }
            GrabEvent::Modifiers(modifiers) => {
                self.keyboard.set_modifiers(modifiers);
                // While we hold the grab, the application only learns about modifier
                // changes through our virtual keyboard.
                if self.sync_vk_keymap() {
                    self.vk.send_modifiers(
                        modifiers.depressed,
                        modifiers.latched,
                        modifiers.locked,
                        modifiers.group,
                    );
                }
            }
            GrabEvent::RepeatInfo(repeat) => self.keyboard.set_repeat_info(repeat),
            GrabEvent::Key { time, key, state } => self.on_key(time, key, state),
        }
    }

    fn on_key(&mut self, time: u32, key: u32, state: u32) {
        match state {
            KEY_STATE_PRESSED => {
                // No engine is attached yet: every key goes straight to the application.
                self.forward_key(time, key, KEY_STATE_PRESSED);
                self.keys.press_forwarded(key);
            }
            KEY_STATE_RELEASED => match self.keys.release(key) {
                ReleaseAction::Forward => self.forward_key(time, key, KEY_STATE_RELEASED),
                ReleaseAction::Swallow => {}
            },
            // The application repeats forwarded keys on its own.
            KEY_STATE_REPEATED => {}
            _ => {}
        }
    }

    fn forward_key(&mut self, time: u32, key: u32, state: u32) {
        if self.sync_vk_keymap() {
            self.vk.send_key(time, key, state);
        } else {
            warn!(
                seat = self.name,
                "dropping a key: no keymap for the virtual keyboard yet"
            );
        }
    }

    /// Uploads the grab's keymap to the virtual keyboard if it changed. Returns whether
    /// the virtual keyboard has a keymap and may receive keys and modifiers.
    fn sync_vk_keymap(&mut self) -> bool {
        let Some(text) = self.keyboard.keymap_text() else {
            return self.vk_keymap.is_some();
        };
        if self.vk_keymap.as_deref() == Some(text) {
            return true;
        }
        match keyboard::keymap_memfd(text) {
            Ok((fd, size)) => {
                self.vk
                    .send_keymap(KEYMAP_FORMAT_XKB_V1, &Rc::new(fd), size);
                self.vk_keymap = Some(text.to_owned());
                let m = self.keyboard.modifiers();
                self.vk
                    .send_modifiers(m.depressed, m.latched, m.locked, m.group);
                true
            }
            Err(e) => {
                error!(seat = self.name, "cannot upload the keymap: {e}");
                self.vk_keymap.is_some()
            }
        }
    }

    /// Gives everything back to the compositor.
    pub fn shutdown(&mut self) {
        self.release_grab();
        if let Some(im) = self.im.take() {
            im.unset_handler();
            im.send_destroy();
        }
        self.vk.send_destroy();
    }
}

fn now_ms() -> u32 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    // Wayland timestamps are 32-bit milliseconds that wrap around.
    (now.tv_sec as u64)
        .wrapping_mul(1000)
        .wrapping_add(now.tv_nsec as u64 / 1_000_000) as u32
}
