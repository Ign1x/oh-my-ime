//! One seat's input method session.
//!
//! The session owns the seat's `zwp_input_method_v2`, keyboard grab and virtual
//! keyboard. It reports focus changes and keys to the core as [`FrontendEvent`]s and
//! carries out the core's decisions: forwarding or swallowing keys and applying the
//! actions engines ask for.

use std::rc::Rc;
use std::time::{Duration, Instant};

use omime_proto::engine::{Action, ContextId, ContextInfo, KeyEvent, Preedit};
use tracing::{debug, error, info, warn};
use wl_proxy::object::{Object, ObjectCoreApi};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::ZwpInputMethodV2;
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wl_proxy::protocols::wayland::wl_seat::WlSeat;

use super::{
    EventQueue, FrontendEvent, GrabEvent, GrabEvents, ImEvent, ImEvents, KeyRef, Outputs, SeatId,
};
use crate::ime_state::ImState;
use crate::keyboard::{self, KeyTracker, Keyboard, ReleaseAction};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const KEY_STATE_RELEASED: u32 = 0;
const KEY_STATE_PRESSED: u32 = 1;
const KEY_STATE_REPEATED: u32 = 2;

const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Key repeat for a key an engine consumed. Forwarded keys are repeated by the
/// application itself.
#[derive(Debug, Clone, Copy)]
struct Repeat {
    keycode: u32,
    next: Instant,
    interval: Duration,
}

/// The input method, keyboard grab and virtual keyboard of one seat.
pub struct SeatSession {
    id: SeatId,
    name: String,
    seat: Rc<WlSeat>,
    queue: EventQueue,
    outputs: Outputs,

    im: Option<Rc<ZwpInputMethodV2>>,
    im_generation: u32,
    ime: ImState,
    /// When to request the input method again after `unavailable`.
    retry: Option<(Instant, Duration)>,
    /// The focused, non-sensitive text field, if any.
    ctx: Option<ContextId>,

    grab: Option<Rc<ZwpInputMethodKeyboardGrabV2>>,
    grab_generation: u32,
    keyboard: Keyboard,
    keys: KeyTracker,
    /// Keys physically held right now, as reported by the grab. Engines answer later
    /// than the release may arrive; repeats only start for keys still held.
    held: std::collections::BTreeSet<u32>,
    repeat: Option<Repeat>,
    /// The compositor repeats keys itself; don't synthesise repeats then.
    compositor_repeats: bool,

    vk: Rc<ZwpVirtualKeyboardV1>,
    /// The keymap last uploaded to the virtual keyboard. Nothing may be sent to the
    /// virtual keyboard before a keymap: that is a fatal protocol error.
    vk_keymap: Option<String>,

    /// The preedit shown in the focused field. Every commit replaces the preedit, so it
    /// is re-sent each time.
    preedit: Preedit,
}

impl std::fmt::Debug for SeatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeatSession")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("ctx", &self.ctx)
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
        outputs: Outputs,
    ) -> Self {
        let vk = vk_manager.new_send_create_virtual_keyboard(&seat);
        vk.set_forward_to_client(false);
        let mut session = Self {
            id,
            name: format!("seat#{id}"),
            seat,
            queue,
            outputs,
            im: None,
            im_generation: 0,
            ime: ImState::default(),
            retry: None,
            ctx: None,
            grab: None,
            grab_generation: 0,
            keyboard: Keyboard::new(),
            keys: KeyTracker::default(),
            held: std::collections::BTreeSet::new(),
            repeat: None,
            compositor_repeats: false,
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

    /// The focused, non-sensitive text field.
    pub fn context(&self) -> Option<ContextId> {
        self.ctx
    }

    pub fn deadline(&self) -> Option<Instant> {
        let retry = self.retry.map(|(at, _)| at);
        let repeat = self.repeat.map(|r| r.next);
        retry.into_iter().chain(repeat).min()
    }

    pub fn on_timer(&mut self, now: Instant, im_manager: &Rc<ZwpInputMethodManagerV2>) {
        if self.im.is_none() && self.retry.is_some_and(|(at, _)| at <= now) {
            info!(seat = self.name, "trying to become the input method again");
            self.request_input_method(im_manager);
        }
        if let Some(repeat) = &mut self.repeat
            && repeat.next <= now
        {
            repeat.next = now + repeat.interval;
            let keycode = repeat.keycode;
            self.emit_key(now_ms(), keycode, false, true);
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

    pub fn handle_im(&mut self, generation: u32, event: ImEvent, next_ctx: &mut ContextId) {
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
            ImEvent::Done => self.on_done(next_ctx),
            ImEvent::Unavailable => self.on_unavailable(),
        }
    }

    fn on_unavailable(&mut self) {
        self.end_context();
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

    fn context_info(&self) -> ContextInfo {
        let current = self.ime.current();
        ContextInfo {
            app_id: None,
            content_type: current.content_type,
            surrounding: current
                .surrounding
                .as_ref()
                .map(|s| (s.text.clone(), s.cursor, s.anchor)),
            cursor_rect: None,
        }
    }

    fn on_done(&mut self, next_ctx: &mut ContextId) {
        let applied = self.ime.done();
        if self.retry.take().is_some() {
            info!(seat = self.name, "became the input method");
        }
        let sensitive = self.ime.current().content_type.is_sensitive();
        let wanted = applied.is_active && !sensitive;
        if applied.activated {
            // Activation resets the text input; whatever we showed is gone.
            self.preedit = Preedit::default();
        }

        if self.ctx.is_some() && (!wanted || applied.activated) {
            self.end_context();
        }
        if wanted && self.ctx.is_none() {
            *next_ctx += 1;
            let ctx = *next_ctx;
            self.ctx = Some(ctx);
            debug!(seat = self.name, ctx, "text field focused");
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::Activated {
                    seat: self.id,
                    ctx,
                    info: self.context_info(),
                });
        } else if let Some(ctx) = self.ctx {
            self.outputs.borrow_mut().push_back(FrontendEvent::Updated {
                ctx,
                info: self.context_info(),
                reset: applied.changed_by_other,
            });
        }
        if applied.is_active && sensitive && !applied.was_active {
            debug!(
                seat = self.name,
                "sensitive text field focused; staying out of it"
            );
        }

        if wanted && self.grab.is_none() {
            self.take_grab();
        } else if !wanted && self.grab.is_some() {
            self.release_grab();
        }

        // Hyprland only sends text-input-v3 `done` after our commit, and every commit
        // replaces the preedit. So always commit, re-sending the preedit we keep.
        self.commit(applied.serial);
    }

    /// Forgets the focused text field and tells the core.
    fn end_context(&mut self) {
        self.repeat = None;
        if let Some(ctx) = self.ctx.take() {
            debug!(seat = self.name, ctx, "text field left");
            self.preedit = Preedit::default();
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::Deactivated { ctx });
        }
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
        self.repeat = None;
        self.held.clear();
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
                    debug!(
                        seat = self.name,
                        format, "ignoring a keymap in another format"
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
            GrabEvent::Key { time, key, state } => match state {
                KEY_STATE_PRESSED => {
                    self.held.insert(key);
                    if self.repeat.is_some_and(|r| r.keycode != key) {
                        self.repeat = None;
                    }
                    self.emit_key(time, key, false, false);
                }
                KEY_STATE_RELEASED => {
                    self.held.remove(&key);
                    if self.repeat.is_some_and(|r| r.keycode == key) {
                        self.repeat = None;
                    }
                    self.emit_key(time, key, true, false);
                }
                KEY_STATE_REPEATED => {
                    self.compositor_repeats = true;
                    self.repeat = None;
                    // Only keys an engine consumed need repeats from us; the
                    // application repeats keys it got itself.
                    if !self.keys.is_forwarded(key) {
                        self.emit_key(time, key, false, true);
                    }
                }
                _ => {}
            },
        }
    }

    /// Hands a key to the core. Without a focused field it is passed through directly.
    fn emit_key(&mut self, time: u32, keycode: u32, released: bool, repeat: bool) {
        let Some(ctx) = self.ctx else {
            let key = KeyRef {
                seat: self.id,
                ctx: 0,
                keycode,
                time,
                released,
                repeat,
            };
            self.finish_key(&key, false);
            return;
        };
        let decoded = self.keyboard.decode(keycode);
        let event = KeyEvent {
            keysym: decoded.as_ref().map_or(0, |d| d.keysym),
            keycode,
            modifiers: self.keyboard.modifier_bits(),
            time,
            text: decoded.map(|d| d.utf8).unwrap_or_default(),
            released,
            repeat,
        };
        let key = KeyRef {
            seat: self.id,
            ctx,
            keycode,
            time,
            released,
            repeat,
        };
        self.outputs
            .borrow_mut()
            .push_back(FrontendEvent::Key { key, event });
    }

    /// Carries out the core's decision about a key.
    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        if key.ctx != 0 && Some(key.ctx) != self.ctx {
            // Focus moved on while the engine was thinking; the key is meaningless now.
            // Releases of keys the application holds were sent when the grab ended.
            return;
        }
        let code = key.keycode;
        match (key.released, key.repeat, handled) {
            (true, ..) => match self.keys.release(code) {
                ReleaseAction::Forward => self.send_key(key.time, code, KEY_STATE_RELEASED),
                ReleaseAction::Swallow => {}
            },
            (false, false, true) => {
                self.keys.press_consumed(code);
                self.start_repeat(code);
            }
            (false, false, false) => {
                self.send_key(key.time, code, KEY_STATE_PRESSED);
                self.keys.press_forwarded(code);
            }
            (false, true, true) => {}
            (false, true, false) => {
                // The engine consumed the press but not this repeat (e.g. Backspace in
                // an empty preedit): the application gets a full key stroke.
                self.send_key(key.time, code, KEY_STATE_PRESSED);
                self.send_key(key.time, code, KEY_STATE_RELEASED);
            }
        }
    }

    fn start_repeat(&mut self, keycode: u32) {
        if !self.held.contains(&keycode) {
            // Released while the engine was deciding.
            return;
        }
        let info = self.keyboard.repeat_info();
        let repeats = self.keyboard.decode(keycode).is_some_and(|d| d.repeats);
        if self.compositor_repeats || info.rate <= 0 || !repeats {
            return;
        }
        let delay = Duration::from_millis(u64::try_from(info.delay.max(0)).unwrap_or(0));
        self.repeat = Some(Repeat {
            keycode,
            next: Instant::now() + delay,
            interval: Duration::from_secs(1) / info.rate.unsigned_abs(),
        });
    }

    /// Applies an engine's actions to the focused field `ctx`.
    pub fn apply_actions(&mut self, ctx: ContextId, actions: &[Action]) {
        if Some(ctx) != self.ctx || actions.is_empty() {
            return;
        }
        let mut batch = Batch::default();
        for action in actions {
            match action {
                Action::Commit(text) => {
                    batch.commit.push_str(text);
                    batch.preedit = Some(Preedit::default());
                }
                Action::Preedit(preedit) => batch.preedit = Some(preedit.clone()),
                Action::DeleteSurrounding { before, after } => {
                    if batch.delete.is_some() {
                        self.flush_batch(std::mem::take(&mut batch));
                    }
                    batch.delete = Some((*before, *after));
                }
                Action::ForwardKey { keycode, pressed } => {
                    // Text before the key must reach the application first.
                    self.flush_batch(std::mem::take(&mut batch));
                    let state = if *pressed {
                        self.keys.press_forwarded(*keycode);
                        KEY_STATE_PRESSED
                    } else {
                        self.keys.release(*keycode);
                        KEY_STATE_RELEASED
                    };
                    self.send_key(now_ms(), *keycode, state);
                }
            }
        }
        self.flush_batch(batch);
    }

    /// Removes the preedit from the focused field `ctx`.
    pub fn clear_preedit(&mut self, ctx: ContextId) {
        if Some(ctx) == self.ctx && !self.preedit.text.is_empty() {
            self.preedit = Preedit::default();
            self.commit(self.ime.serial());
        }
    }

    /// Sends one `zwp_input_method_v2.commit` worth of changes.
    fn flush_batch(&mut self, batch: Batch) {
        if batch.is_empty() {
            return;
        }
        let Some(im) = self.im.clone() else { return };
        if let Some((before, after)) = batch.delete {
            im.send_delete_surrounding_text(before, after);
        }
        if !batch.commit.is_empty() {
            im.send_commit_string(&batch.commit);
        }
        if let Some(preedit) = batch.preedit {
            self.preedit = preedit;
        }
        self.commit(self.ime.serial());
    }

    fn send_key(&mut self, time: u32, key: u32, state: u32) {
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
        self.end_context();
        self.release_grab();
        if let Some(im) = self.im.take() {
            im.unset_handler();
            im.send_destroy();
        }
        self.vk.send_destroy();
    }
}

/// Changes that go out in a single `zwp_input_method_v2.commit`.
#[derive(Debug, Default)]
struct Batch {
    commit: String,
    delete: Option<(u32, u32)>,
    preedit: Option<Preedit>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.commit.is_empty() && self.delete.is_none() && self.preedit.is_none()
    }
}

fn now_ms() -> u32 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    // Wayland timestamps are 32-bit milliseconds that wrap around.
    (now.tv_sec as u64)
        .wrapping_mul(1000)
        .wrapping_add(now.tv_nsec as u64 / 1_000_000) as u32
}
