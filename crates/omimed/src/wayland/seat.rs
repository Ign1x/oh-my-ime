//! One seat's input method session.
//!
//! The session owns the seat's `zwp_input_method_v2`, keyboard grab and, while it
//! holds the grab, a virtual keyboard. It reports focus changes and keys to the core as
//! [`FrontendEvent`]s and carries out the core's decisions: forwarding or swallowing
//! keys and applying the actions engines ask for.

use std::collections::{BTreeSet, VecDeque};
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

use super::{EventQueue, GrabEvent, GrabEvents, ImEvent, ImEvents, KeyRef, Outputs, SeatId};
use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
use crate::ime_state::{ImState, SurroundingText};
use crate::keyboard::{self, KeyTracker, Keyboard, ModifierState, ReleaseAction, RepeatInfo};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const KEY_STATE_RELEASED: u32 = 0;
const KEY_STATE_PRESSED: u32 = 1;
const KEY_STATE_REPEATED: u32 = 2;

const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

// Applications normally report their new surrounding text after a commit. Wait
// for that update before sending another text change with the previous serial.
// A client that stops reporting must not stall typing indefinitely.
const TEXT_FEEDBACK_WAIT: Duration = Duration::from_millis(200);

/// Maximum UTF-8 string length in the input-method-v2 and text-input-v3 protocols.
pub(super) const MAX_TEXT_BYTES: usize = 4000;
/// Bound text retained while earlier output waits for application feedback.
pub(super) const MAX_PENDING_COMMIT_BYTES: usize = 64 * 1024;
const MAX_PENDING_OUTPUTS: usize = 256;

/// A change of the keyboard a grab reported.
#[derive(Debug)]
pub enum KeyboardChange {
    Keymap(Rc<str>),
    Repeat(RepeatInfo),
    Modifiers(ModifierState),
}

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
    /// Whether another input method held the seat when omimed last asked for it, and
    /// omimed has not served a text field since.
    taken: bool,
    /// The focused, non-sensitive text field, if any.
    ctx: Option<ContextId>,

    grab: Option<Rc<ZwpInputMethodKeyboardGrabV2>>,
    grab_generation: u32,
    keyboard: Keyboard,
    keymap: Option<Rc<str>>,
    keys: KeyTracker,
    /// Presses actually sent, distinct from decisions waiting in pending_output.
    sent_keys: std::collections::BTreeSet<u32>,
    /// Keys physically held right now, as reported by the grab. Engines answer later
    /// than the release may arrive; repeats only start for keys still held.
    held: std::collections::BTreeSet<u32>,
    repeat: Option<Repeat>,
    /// The compositor repeats keys itself; don't synthesise repeats then.
    compositor_repeats: bool,

    vk_manager: Rc<ZwpVirtualKeyboardManagerV1>,
    /// The virtual keyboard keys and modifiers reach the application through. It
    /// lives as long as the grab: see [`Self::release_grab`].
    vk: Option<Rc<ZwpVirtualKeyboardV1>>,
    /// The keymap last uploaded to the virtual keyboard. Nothing may be sent to the
    /// virtual keyboard before a keymap: that is a fatal protocol error.
    vk_keymap: Option<String>,

    /// The preedit shown in the focused field. Every commit replaces the preedit, so it
    /// is re-sent each time.
    preedit: Preedit,
    text_feedback: Option<TextFeedback>,
    feedback_enabled: bool,
    pending_output: VecDeque<PendingOutput>,
    /// The app id of the focused window, when the compositor reports it.
    app_id: Option<String>,
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
            taken: false,
            ctx: None,
            grab: None,
            grab_generation: 0,
            keyboard: Keyboard::new(),
            keymap: None,
            keys: KeyTracker::default(),
            sent_keys: std::collections::BTreeSet::new(),
            held: std::collections::BTreeSet::new(),
            repeat: None,
            compositor_repeats: false,
            vk_manager: vk_manager.clone(),
            vk: None,
            vk_keymap: None,
            preedit: Preedit::default(),
            text_feedback: None,
            feedback_enabled: true,
            pending_output: VecDeque::new(),
            app_id: None,
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
    /// Whether omimed is this seat's input method (rather than waiting for another one
    /// to leave).
    pub fn is_input_method(&self) -> bool {
        self.im.is_some() && self.retry.is_none()
    }

    pub fn input_method(&self) -> Option<&Rc<ZwpInputMethodV2>> {
        self.im.as_ref()
    }

    /// The focused, non-sensitive text field.
    pub fn context(&self) -> Option<ContextId> {
        self.ctx
    }

    /// Another application got focus; a focused field now belongs to it.
    pub fn set_app_id(&mut self, app_id: Option<String>) {
        if self.app_id == app_id {
            return;
        }
        self.app_id = app_id;
        if let Some(ctx) = self.ctx {
            self.outputs.borrow_mut().push_back(FrontendEvent::Updated {
                ctx,
                info: self.context_info(),
                reset: false,
            });
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        let retry = self.retry.map(|(at, _)| at);
        let repeat = self.repeat.map(|r| r.next);
        retry
            .into_iter()
            .chain(repeat)
            .chain(
                self.text_feedback
                    .as_ref()
                    .map(|feedback| feedback.deadline),
            )
            .min()
    }

    pub fn on_timer(&mut self, now: Instant, im_manager: &Rc<ZwpInputMethodManagerV2>) {
        if self
            .text_feedback
            .as_ref()
            .is_some_and(|feedback| feedback.deadline <= now)
        {
            // Never resend the previous commit: the protocol has no acceptance
            // acknowledgment. Only release output that has not been sent yet.
            self.text_feedback = None;
            self.feedback_enabled = false;
            self.drain_output();
        }
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

    pub fn handle_im(&mut self, generation: u32, event: ImEvent, context_ids: &ContextIds) {
        if generation != self.im_generation || self.im.is_none() {
            return;
        }
        match event {
            ImEvent::Activate => {
                if std::mem::take(&mut self.taken) {
                    self.outputs
                        .borrow_mut()
                        .push_back(FrontendEvent::SeatTaken {
                            seat: self.name.clone(),
                            taken: false,
                        });
                }
                self.ime.activate();
            }
            ImEvent::Deactivate => self.ime.deactivate(),
            ImEvent::SurroundingText {
                text,
                cursor,
                anchor,
            } => self.ime.surrounding_text(text, cursor, anchor),
            ImEvent::TextChangeCause(cause) => self.ime.text_change_cause(cause),
            ImEvent::ContentType { hints, purpose } => self.ime.content_type(hints, purpose),
            ImEvent::Done => self.on_done(context_ids),
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
        if !std::mem::replace(&mut self.taken, true) {
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::SeatTaken {
                    seat: self.name.clone(),
                    taken: true,
                });
        }
    }

    fn context_info(&self) -> ContextInfo {
        let current = self.ime.current();
        ContextInfo {
            app_id: self.app_id.clone(),
            content_type: current.content_type,
            surrounding: current
                .surrounding
                .as_ref()
                .map(|s| (s.text.clone(), s.cursor, s.anchor)),
            cursor_rect: None,
            frontend: Some("wayland".into()),
            // text-input-v3 applications show the preedit.
            client_preedit: true,
        }
    }

    fn on_done(&mut self, context_ids: &ContextIds) {
        let before = self.ime.current().surrounding.clone();
        let applied = self.ime.done();
        if before != self.ime.current().surrounding {
            self.feedback_enabled = true;
        }
        if self.text_feedback.as_ref().is_some_and(|feedback| {
            self.ime.current().surrounding.as_ref() != Some(&feedback.before)
        }) {
            self.text_feedback = None;
        }
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
            let ctx = context_ids.next();
            self.ctx = Some(ctx);
            debug!(seat = self.name, ctx, "text field focused");
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::Activated {
                    origin: Origin::Wayland(self.id),
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
        self.drain_output();
    }

    /// Forgets the focused text field and tells the core.
    fn end_context(&mut self) {
        self.repeat = None;
        self.text_feedback = None;
        self.feedback_enabled = true;
        self.pending_output.clear();
        // Reactivation can keep the grab. Any delivered press still needs its
        // release when the old context's buffered output is discarded.
        self.release_sent_keys();
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
        let vk = self.vk_manager.new_send_create_virtual_keyboard(&self.seat);
        vk.set_forward_to_client(false);
        self.vk = Some(vk);
        self.vk_keymap = None;
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

    /// Releases the grab and destroys the virtual keyboard. Keys the application still
    /// sees as pressed are released on the virtual keyboard first; afterwards their
    /// real releases would bypass us.
    fn release_grab(&mut self) {
        let Some(grab) = self.grab.take() else { return };
        self.repeat = None;
        self.held.clear();
        self.release_sent_keys();
        grab.unset_handler();
        grab.send_release();
        // The virtual keyboard holds the modifiers it last passed on, say Ctrl when the
        // field is left with Ctrl+W. Hyprland merges the state of all keyboards and
        // skips an input method's virtual keyboard only while the input method holds a
        // grab: with `share_states = 1`, a Ctrl kept after the grab would stay held for
        // every application, whatever the real keyboards do. Telling the virtual
        // keyboard that nothing is held is no better: the focused application would
        // take Ctrl as released while the user still holds it. Gone, the keyboard holds
        // nothing, and applications keep the true state until the real keyboards
        // change it. Fcitx 5 does the same.
        if let Some(vk) = self.vk.take() {
            vk.send_destroy();
        }
        self.vk_keymap = None;
        debug!(seat = self.name, "keyboard released");
    }

    fn release_sent_keys(&mut self) {
        self.keys.take_forwarded();
        let forwarded = std::mem::take(&mut self.sent_keys);
        if let Some(vk) = &self.vk
            && self.vk_keymap.is_some()
        {
            for key in forwarded {
                vk.send_key(now_ms(), key, KEY_STATE_RELEASED);
            }
        }
    }

    /// Handles an event of the keyboard grab. Returns what changed about the keyboard.
    pub fn handle_grab(&mut self, generation: u32, event: GrabEvent) -> Option<KeyboardChange> {
        if generation != self.grab_generation || self.grab.is_none() {
            return None;
        }
        match event {
            GrabEvent::Keymap { format, fd, size } => {
                if format != KEYMAP_FORMAT_XKB_V1 {
                    debug!(
                        seat = self.name,
                        format, "ignoring a keymap in another format"
                    );
                    return None;
                }
                let loaded = keyboard::read_keymap(&fd, size)
                    .map_err(keyboard::KeymapError::from)
                    .and_then(|text| self.keyboard.set_keymap(text));
                match loaded {
                    Ok(true) => {
                        debug!(seat = self.name, "new keymap");
                        self.keymap = self.keyboard.keymap_text().map(Rc::from);
                        return self.keymap.clone().map(KeyboardChange::Keymap);
                    }
                    Ok(false) => {}
                    Err(e) => error!(seat = self.name, "cannot use the keymap: {e}"),
                }
            }
            GrabEvent::Modifiers(modifiers) => {
                self.keyboard.set_modifiers(modifiers);
                // While we hold the grab, the application only learns about modifier
                // changes through our virtual keyboard.
                if self.sync_vk_keymap()
                    && let Some(vk) = &self.vk
                {
                    vk.send_modifiers(
                        modifiers.depressed,
                        modifiers.latched,
                        modifiers.locked,
                        modifiers.group,
                    );
                }
                return Some(KeyboardChange::Modifiers(modifiers));
            }
            GrabEvent::RepeatInfo(repeat) => {
                self.keyboard.set_repeat_info(repeat);
                return Some(KeyboardChange::Repeat(repeat));
            }
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
        None
    }

    /// Hands a key to the core. Without a focused field it is passed through directly.
    fn emit_key(&mut self, time: u32, keycode: u32, released: bool, repeat: bool) {
        let modifiers = self.keyboard.modifiers();
        let Some(ctx) = self.ctx else {
            let key = KeyRef {
                seat: self.id,
                ctx: 0,
                keycode,
                time,
                released,
                repeat,
                modifiers,
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
            modifiers,
        };
        self.outputs.borrow_mut().push_back(FrontendEvent::Key {
            key: frontend::KeyRef::Wayland(key),
            event,
        });
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
                ReleaseAction::Forward => {
                    self.send_key(key.time, code, KEY_STATE_RELEASED, key.modifiers);
                }
                ReleaseAction::Swallow => {}
            },
            (false, false, true) => {
                self.keys.press_consumed(code);
                self.start_repeat(code);
            }
            (false, false, false) => {
                self.send_press(key.time, code, key.modifiers);
            }
            (false, true, true) => {}
            (false, true, false) => {
                // The engine consumed the press but not this repeat (e.g. Backspace in
                // an empty preedit): the application gets a full key stroke.
                if self.send_key(key.time, code, KEY_STATE_PRESSED, key.modifiers) {
                    self.send_key(key.time, code, KEY_STATE_RELEASED, key.modifiers);
                }
            }
        }
    }

    /// Records that a key went to a hosted input method. It forwards what the
    /// application should get through its own virtual keyboard, and repeats keys
    /// itself.
    pub fn finish_delegated(&mut self, key: &KeyRef) {
        if key.ctx != 0 && Some(key.ctx) != self.ctx {
            return;
        }
        let code = key.keycode;
        if key.released {
            // The key is up: an application holding a press the input method forwarded
            // gets the release now, whether or not the input method forwards one too
            // (input methods may keep releases of keys that were part of a hotkey).
            let held = self.keys.is_forwarded(code);
            self.keys.release(code);
            if held {
                self.send_key(key.time, code, KEY_STATE_RELEASED, key.modifiers);
            }
        } else if !key.repeat {
            self.keys.press_consumed(code);
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
        if !output_fits(&self.pending_output, &self.sent_keys, actions) {
            warn!(
                seat = self.name,
                "dropping engine actions: output queue limit exceeded"
            );
            // Releases of earlier accepted presses remain necessary even when
            // this reply's new output cannot fit.
            for action in actions {
                if let Action::ForwardKey {
                    keycode,
                    pressed: false,
                } = action
                {
                    self.forward_key(
                        ctx,
                        &frontend::ForwardedKey {
                            keycode: *keycode,
                            time: now_ms(),
                            pressed: false,
                            modifiers: self.keyboard.modifiers(),
                        },
                    );
                }
            }
            return;
        }
        let mut batch = Batch::default();
        for action in actions {
            match action {
                // Mode confirmations are consumed by Core, never sent to applications.
                Action::InputMode { .. } => {}
                Action::Commit(text) => {
                    batch.commit.push_str(text);
                    batch.preedit = Some(Preedit::default());
                }
                Action::Preedit(preedit) => batch.preedit = Some(bounded_preedit(preedit)),
                Action::DeleteSurrounding { before, after } => {
                    if batch.delete.is_some() {
                        self.flush_batch(std::mem::take(&mut batch));
                    }
                    batch.delete = Some((*before, *after));
                }
                Action::ForwardKey { keycode, pressed } => {
                    // Text before the key must reach the application first.
                    self.flush_batch(std::mem::take(&mut batch));
                    self.forward_key(
                        ctx,
                        &frontend::ForwardedKey {
                            keycode: *keycode,
                            time: now_ms(),
                            pressed: *pressed,
                            modifiers: self.keyboard.modifiers(),
                        },
                    );
                }
            }
        }
        self.flush_batch(batch);
    }

    pub fn forward_key(&mut self, ctx: ContextId, key: &frontend::ForwardedKey) {
        if Some(ctx) != self.ctx {
            return;
        }
        let code = key.keycode;
        if key.pressed && !self.held.contains(&code) {
            // A busy input method may forward the press after the real release.
            // Complete the stroke now so the application cannot keep it held.
            if self.send_key(key.time, code, KEY_STATE_PRESSED, key.modifiers) {
                self.send_key(key.time, code, KEY_STATE_RELEASED, key.modifiers);
            }
            return;
        }
        if key.pressed {
            self.send_press(key.time, code, key.modifiers);
        } else {
            if !self.keys.is_forwarded(code) {
                return;
            }
            self.keys.release(code);
            self.send_key(key.time, code, KEY_STATE_RELEASED, key.modifiers);
        }
    }

    /// Removes the preedit from the focused field `ctx`.
    pub fn clear_preedit(&mut self, ctx: ContextId) {
        if Some(ctx) == self.ctx {
            // An engine switch must not resurrect its buffered preedit, while
            // text it already committed still belongs to this field.
            for output in &mut self.pending_output {
                if let PendingOutput::Batch(batch) = output {
                    batch.preedit = None;
                }
            }
        }
        if Some(ctx) == self.ctx && !self.preedit.text.is_empty() {
            self.preedit = Preedit::default();
            self.commit(self.ime.serial());
        }
    }

    /// Queues text changes before any keys that follow them.
    fn flush_batch(&mut self, batch: Batch) {
        if batch.is_empty() {
            return;
        }
        if !enqueue_output(
            &mut self.pending_output,
            &self.sent_keys,
            PendingOutput::Batch(batch),
        ) {
            warn!(
                seat = self.name,
                "dropping engine actions: output queue limit exceeded"
            );
            return;
        }
        self.drain_output();
    }

    fn drain_output(&mut self) {
        loop {
            // Do not delay the release of a press already sent while waiting for
            // the application's response to that press.
            if self.text_feedback.is_some()
                && !matches!(self.pending_output.front(), Some(PendingOutput::Key {
                    key, state: KEY_STATE_RELEASED, ..
                }) if self.sent_keys.contains(key))
            {
                break;
            }
            let Some(output) = self.pending_output.pop_front() else {
                break;
            };
            match output {
                PendingOutput::Batch(batch) => self.send_batch(batch),
                PendingOutput::Key {
                    time,
                    key,
                    state,
                    modifiers,
                    keymap,
                    changes_text,
                } => {
                    self.send_key_now(time, key, state, modifiers, keymap.as_deref(), changes_text)
                }
            }
        }
    }

    fn send_batch(&mut self, mut batch: Batch) {
        let Some(im) = self.im.clone() else { return };
        if let Some(rest) = batch.split() {
            // This replaces the batch just popped, using its already reserved slot.
            self.pending_output.push_front(PendingOutput::Batch(rest));
        }
        let changes_text = !batch.commit.is_empty()
            || batch
                .delete
                .is_some_and(|(before, after)| before != 0 || after != 0)
            || (batch
                .preedit
                .as_ref()
                .is_some_and(|preedit| !preedit.text.is_empty())
                && self
                    .ime
                    .current()
                    .surrounding
                    .as_ref()
                    .is_some_and(|text| text.cursor != text.anchor));
        if changes_text {
            self.wait_for_text_feedback();
        }
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

    fn send_press(&mut self, time: u32, key: u32, modifiers: ModifierState) {
        if self.send_key(time, key, KEY_STATE_PRESSED, modifiers) {
            self.keys.press_forwarded(key);
        } else if !self.keys.is_forwarded(key) {
            // Swallow the release of a rejected press, but retain the release
            // obligation if this was a repeated press of an already held key.
            self.keys.press_consumed(key);
        }
    }

    fn send_key(&mut self, time: u32, key: u32, state: u32, modifiers: ModifierState) -> bool {
        let changes_text = state != KEY_STATE_RELEASED
            && self.keyboard.decode(key).is_some_and(|key| {
                !key.utf8.is_empty()
                    || matches!(
                        key.keysym,
                        0xff08 | 0xff09 | 0xff0d | 0xff8d | 0xffff | 0xff50..=0xff58
                    )
            });
        let keymap = self.keymap.clone();
        let output = PendingOutput::Key {
            time,
            key,
            state,
            modifiers,
            keymap: keymap.clone(),
            changes_text,
        };
        if !enqueue_output(&mut self.pending_output, &self.sent_keys, output) {
            if state == KEY_STATE_RELEASED {
                // Accepted presses reserve space for their releases. Only a
                // release without an outstanding press can get here (e.g. a key
                // pressed before our grab); send it without growing the queue.
                self.send_key_now(time, key, state, modifiers, keymap.as_deref(), false);
                return true;
            }
            warn!(
                seat = self.name,
                "dropping a key: output queue limit exceeded"
            );
            return false;
        }
        self.drain_output();
        true
    }

    fn send_key_now(
        &mut self,
        time: u32,
        key: u32,
        state: u32,
        modifiers: ModifierState,
        keymap: Option<&str>,
        changes_text: bool,
    ) {
        if self.sync_vk_keymap_text(keymap)
            && let Some(vk) = self.vk.clone()
        {
            if state == KEY_STATE_RELEASED {
                self.sent_keys.remove(&key);
            } else {
                self.sent_keys.insert(key);
                if changes_text {
                    self.wait_for_text_feedback();
                }
            }
            // Engine replies may arrive after later modifier events. Interpret this
            // key with its original state, then restore what is physically held so
            // the replay cannot leave a modifier stuck in the application.
            let current = self.keyboard.modifiers();
            if modifiers != current {
                vk.send_modifiers(
                    modifiers.depressed,
                    modifiers.latched,
                    modifiers.locked,
                    modifiers.group,
                );
            }
            vk.send_key(time, key, state);
            if modifiers != current {
                vk.send_modifiers(
                    current.depressed,
                    current.latched,
                    current.locked,
                    current.group,
                );
            }
        } else {
            warn!(
                seat = self.name,
                "dropping a key: no virtual keyboard with a keymap"
            );
        }
    }

    fn wait_for_text_feedback(&mut self) {
        if self.feedback_enabled {
            self.text_feedback =
                self.ime
                    .current()
                    .surrounding
                    .clone()
                    .map(|before| TextFeedback {
                        before,
                        deadline: Instant::now() + TEXT_FEEDBACK_WAIT,
                    });
        }
    }

    /// Uploads the grab's keymap to the virtual keyboard if it changed. Returns whether
    /// there is a virtual keyboard with a keymap, which may receive keys and modifiers.
    fn sync_vk_keymap(&mut self) -> bool {
        let text = self.keymap.clone();
        self.sync_vk_keymap_text(text.as_deref())
    }

    fn sync_vk_keymap_text(&mut self, text: Option<&str>) -> bool {
        let Some(vk) = self.vk.clone() else {
            return false;
        };
        let Some(text) = text else {
            return self.vk_keymap.is_some();
        };
        if self.vk_keymap.as_deref() == Some(text) {
            return true;
        }
        match keyboard::keymap_memfd(text) {
            Ok((fd, size)) => {
                vk.send_keymap(KEYMAP_FORMAT_XKB_V1, &Rc::new(fd), size);
                self.vk_keymap = Some(text.to_owned());
                let m = self.keyboard.modifiers();
                vk.send_modifiers(m.depressed, m.latched, m.locked, m.group);
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
    }
}

/// Changes split into `zwp_input_method_v2.commit` cycles when sent.
#[derive(Debug, Default)]
struct Batch {
    commit: String,
    delete: Option<(u32, u32)>,
    preedit: Option<Preedit>,
}

struct TextFeedback {
    before: SurroundingText,
    deadline: Instant,
}

enum PendingOutput {
    Batch(Batch),
    Key {
        time: u32,
        key: u32,
        state: u32,
        modifiers: ModifierState,
        keymap: Option<Rc<str>>,
        changes_text: bool,
    },
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.commit.is_empty() && self.delete.is_none() && self.preedit.is_none()
    }

    /// Keep one message here and return its continuation. Delete once, and show
    /// the final preedit only after all the committed text reaches the field.
    fn split(&mut self) -> Option<Self> {
        if self.commit.len() <= MAX_TEXT_BYTES {
            return None;
        }
        let end = text_prefix(&self.commit, MAX_TEXT_BYTES).len();
        Some(Self {
            commit: self.commit.split_off(end),
            delete: None,
            preedit: self.preedit.replace(Preedit::default()),
        })
    }
}

fn text_prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn bounded_preedit(preedit: &Preedit) -> Preedit {
    if preedit.text.len() > MAX_TEXT_BYTES {
        warn!(
            bytes = preedit.text.len(),
            "truncating oversized engine preedit"
        );
    }
    let text = text_prefix(&preedit.text, MAX_TEXT_BYTES);
    let cursor = |offset: i32| {
        if offset < 0 {
            -1
        } else {
            text_prefix(text, offset as usize).len() as i32
        }
    };
    Preedit {
        text: text.to_owned(),
        cursor_begin: cursor(preedit.cursor_begin),
        cursor_end: cursor(preedit.cursor_end),
    }
}

/// Keys that will still need releases after the queued output is sent.
fn pending_releases(pending: &VecDeque<PendingOutput>, sent: &BTreeSet<u32>) -> BTreeSet<u32> {
    let mut releases = sent.clone();
    for output in pending {
        if let PendingOutput::Key { key, state, .. } = output {
            if *state == KEY_STATE_RELEASED {
                releases.remove(key);
            } else {
                releases.insert(*key);
            }
        }
    }
    releases
}

fn enqueue_output(
    pending: &mut VecDeque<PendingOutput>,
    sent: &BTreeSet<u32>,
    output: PendingOutput,
) -> bool {
    let mut releases = pending_releases(pending, sent);
    if let PendingOutput::Key { key, state, .. } = &output {
        if *state == KEY_STATE_RELEASED {
            releases.remove(key);
        } else {
            releases.insert(*key);
        }
    }
    // Every accepted press owns a slot for its eventual release, even after the
    // press itself has been sent. Text and other keys cannot consume those slots.
    if pending.len() + 1 + releases.len() > MAX_PENDING_OUTPUTS {
        return false;
    }
    pending.push_back(output);
    true
}

fn output_fits(
    pending: &VecDeque<PendingOutput>,
    sent: &BTreeSet<u32>,
    actions: &[Action],
) -> bool {
    // A forwarded key can queue both a press and its already-arrived release.
    // Splitting is lazy, so only one continuation is retained for each batch.
    if actions
        .len()
        .saturating_mul(2)
        .saturating_add(pending.len())
        .saturating_add(pending_releases(pending, sent).len())
        > MAX_PENDING_OUTPUTS
    {
        return false;
    }
    let mut bytes = pending.iter().fold(0usize, |bytes, output| {
        bytes.saturating_add(match output {
            PendingOutput::Batch(batch) => batch.commit.len(),
            PendingOutput::Key { .. } => 0,
        })
    });
    for action in actions {
        if let Action::Commit(text) = action {
            bytes = bytes.saturating_add(text.len());
        }
    }
    bytes <= MAX_PENDING_COMMIT_BYTES
}

fn now_ms() -> u32 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    // Wayland timestamps are 32-bit milliseconds that wrap around.
    (now.tv_sec as u64)
        .wrapping_mul(1000)
        .wrapping_add(now.tv_nsec as u64 / 1_000_000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_output(key: u32, state: u32) -> PendingOutput {
        PendingOutput::Key {
            time: 0,
            key,
            state,
            modifiers: ModifierState::default(),
            keymap: None,
            changes_text: false,
        }
    }

    #[test]
    fn saturated_output_keeps_split_text_and_reserves_sent_and_queued_releases() {
        let text = "x".repeat(MAX_PENDING_COMMIT_BYTES);
        let mut first = Batch {
            commit: text.clone(),
            ..Batch::default()
        };
        let rest = first.split().expect("long commit has a continuation");
        let mut pending = VecDeque::new();
        let mut sent = BTreeSet::from([59]);
        assert!(enqueue_output(
            &mut pending,
            &sent,
            PendingOutput::Batch(rest)
        ));
        assert!(enqueue_output(
            &mut pending,
            &sent,
            key_output(30, KEY_STATE_PRESSED)
        ));
        let mut accepted = 0;
        for _ in 0..400 {
            if enqueue_output(&mut pending, &sent, key_output(60, KEY_STATE_PRESSED)) {
                accepted += 1;
                assert!(enqueue_output(
                    &mut pending,
                    &sent,
                    key_output(60, KEY_STATE_RELEASED)
                ));
            }
            assert!(pending.len() + pending_releases(&pending, &sent).len() <= MAX_PENDING_OUTPUTS);
        }
        assert_eq!(accepted, 126);
        assert!(!output_fits(
            &pending,
            &sent,
            &[Action::Commit("later".into())]
        ));
        assert!(enqueue_output(
            &mut pending,
            &sent,
            key_output(59, KEY_STATE_RELEASED)
        ));
        assert!(enqueue_output(
            &mut pending,
            &sent,
            key_output(30, KEY_STATE_RELEASED)
        ));
        assert_eq!(pending.len(), MAX_PENDING_OUTPUTS);
        let Some(PendingOutput::Batch(rest)) = pending.pop_front() else {
            panic!("accepted text must precede the keys");
        };
        assert_eq!(first.commit + &rest.commit, text);
        for output in pending {
            let PendingOutput::Key { key, state, .. } = output else {
                panic!("expected a key");
            };
            if state == KEY_STATE_RELEASED {
                assert!(sent.remove(&key), "release without a press");
            } else {
                assert!(sent.insert(key), "press without a release");
            }
        }
        assert!(sent.is_empty());
    }

    #[test]
    fn text_cannot_take_the_last_slot_reserved_for_a_release() {
        let mut pending = VecDeque::new();
        let sent = BTreeSet::from([59]);
        for _ in 0..MAX_PENDING_OUTPUTS - 1 {
            assert!(enqueue_output(
                &mut pending,
                &sent,
                PendingOutput::Batch(Batch {
                    commit: "x".into(),
                    ..Batch::default()
                })
            ));
        }
        assert!(!enqueue_output(
            &mut pending,
            &sent,
            PendingOutput::Batch(Batch {
                commit: "rejected".into(),
                ..Batch::default()
            })
        ));
        assert!(!enqueue_output(
            &mut pending,
            &sent,
            key_output(60, KEY_STATE_PRESSED)
        ));
        assert!(enqueue_output(
            &mut pending,
            &sent,
            key_output(59, KEY_STATE_RELEASED)
        ));
        assert_eq!(pending.len(), MAX_PENDING_OUTPUTS);
    }

    #[test]
    fn commits_split_at_multibyte_boundaries_without_repeating_changes() {
        for prefix in [3997, 3998, 3999, 4000, 4001, 5000, 8180] {
            for character in ['é', '中', '🙂'] {
                let text = format!(
                    "{}{}end",
                    "x".repeat(prefix),
                    character.to_string().repeat(4000)
                );
                let preedit = Preedit::with_cursor_at_end("next");
                let mut batch = Batch {
                    commit: text.clone(),
                    delete: Some((3, 4)),
                    preedit: Some(preedit.clone()),
                };
                let mut received = String::new();
                let mut deletions = Vec::new();
                loop {
                    let rest = batch.split();
                    assert!(batch.commit.len() <= MAX_TEXT_BYTES);
                    assert!(!batch.commit.is_empty());
                    received.push_str(&batch.commit);
                    deletions.extend(batch.delete);
                    if let Some(rest) = rest {
                        assert_eq!(batch.preedit, Some(Preedit::default()));
                        batch = rest;
                    } else {
                        assert_eq!(batch.preedit, Some(preedit));
                        break;
                    }
                }
                assert_eq!(received, text);
                assert_eq!(deletions, [(3, 4)]);
            }
        }
    }

    #[test]
    fn short_commits_need_no_continuation() {
        for text in [String::new(), "中".repeat(1333), "x".repeat(4000)] {
            let mut batch = Batch {
                commit: text.clone(),
                ..Batch::default()
            };
            assert!(batch.split().is_none());
            assert_eq!(batch.commit, text);
        }
    }

    #[test]
    fn truncated_preedits_keep_cursors_on_utf8_boundaries() {
        let text = format!("{}🙂tail", "中".repeat(1333));
        for (begin, end, wanted_begin, wanted_end) in [
            (4007, 4003, 3999, 3999),
            (3998, 4, 3996, 3),
            (-1, -1, -1, -1),
            (i32::MIN, i32::MAX, -1, 3999),
            (0, 3999, 0, 3999),
        ] {
            let bounded = bounded_preedit(&Preedit {
                text: text.clone(),
                cursor_begin: begin,
                cursor_end: end,
            });
            assert_eq!(bounded.text, "中".repeat(1333));
            assert_eq!(
                (bounded.cursor_begin, bounded.cursor_end),
                (wanted_begin, wanted_end)
            );
        }
        assert_eq!(bounded_preedit(&Preedit::default()), Preedit::default());
        let short = Preedit::with_cursor_at_end("中文");
        assert_eq!(bounded_preedit(&short), short);
    }

    #[test]
    fn replies_cannot_grow_the_pending_output_without_bound() {
        let actions = [Action::Commit("x".repeat(MAX_PENDING_COMMIT_BYTES))];
        let mut pending = VecDeque::new();
        assert!(output_fits(&pending, &BTreeSet::new(), &actions));
        pending.push_back(PendingOutput::Batch(Batch {
            commit: "x".into(),
            ..Batch::default()
        }));
        assert!(!output_fits(&pending, &BTreeSet::new(), &actions));
        assert!(!output_fits(
            &VecDeque::new(),
            &BTreeSet::new(),
            &[Action::Commit("x".repeat(MAX_PENDING_COMMIT_BYTES + 1))]
        ));
        assert!(!output_fits(
            &VecDeque::new(),
            &BTreeSet::new(),
            &vec![
                Action::DeleteSurrounding {
                    before: 1,
                    after: 0
                };
                MAX_PENDING_OUTPUTS
            ]
        ));
    }
}
