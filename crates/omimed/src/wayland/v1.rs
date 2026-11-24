//! The privileged input-method-v1 connection used by KWin.
//!
//! Text state is applied at `commit_state`. Keyboard forwarding instead echoes the
//! serial of the original `wl_keyboard.key`; these serial namespaces are unrelated.

use std::collections::{BTreeSet, VecDeque};
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use omime_proto::engine::{Action, ContextId, ContextInfo, KeyEvent, Preedit};
use omime_proto::{ContentHints, ContentPurpose, ContentType};
use tracing::{debug, warn};
use wl_proxy::object::{Object, ObjectCoreApi};
use wl_proxy::protocols::input_method_unstable_v1::zwp_input_method_context_v1::{
    ZwpInputMethodContextV1, ZwpInputMethodContextV1Handler,
};
use wl_proxy::protocols::input_method_unstable_v1::zwp_input_method_v1::{
    ZwpInputMethodV1, ZwpInputMethodV1Handler,
};
use wl_proxy::protocols::wayland::wl_keyboard::{
    WlKeyboard, WlKeyboardHandler, WlKeyboardKeyState, WlKeyboardKeymapFormat,
};

use super::seat::KeyboardChange;
use super::{Event, EventQueue, KeyRef, Outputs};
use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
use crate::ime_state::SurroundingText;
use crate::keyboard::{self, KeyTracker, Keyboard, ModifierState, ReleaseAction, RepeatInfo};

/// V1 has one input-method global rather than one object per advertised seat.
pub const SEAT: u32 = u32::MAX;
const MAX_PENDING_KEYS: usize = 256;
const MAX_TEXT_BYTES: usize = 4000;

pub enum V1Event {
    Activate(Rc<ZwpInputMethodContextV1>),
    Deactivate(Rc<ZwpInputMethodContextV1>),
    Context {
        generation: u32,
        event: ContextEvent,
    },
    Keyboard {
        generation: u32,
        event: KeyboardEvent,
    },
}

// Neither surrounding text nor key codes belong in logs.
impl std::fmt::Debug for V1Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Activate(_) => "V1Activate",
            Self::Deactivate(_) => "V1Deactivate",
            Self::Context { .. } => "V1Context",
            Self::Keyboard { .. } => "V1Keyboard",
        })
    }
}

pub enum ContextEvent {
    Surrounding(SurroundingText),
    ContentType(ContentType),
    Reset,
    Commit(u32),
}

impl std::fmt::Debug for ContextEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("V1ContextEvent")
    }
}

pub enum KeyboardEvent {
    Keymap {
        format: u32,
        fd: Rc<OwnedFd>,
        size: u32,
    },
    Key {
        serial: u32,
        time: u32,
        code: u32,
        state: u32,
    },
    Modifiers {
        serial: u32,
        state: ModifierState,
    },
    Repeat(RepeatInfo),
}

/// Applied text state. In particular, activation alone does not permit an engine
/// context or keyboard grab before the field's content type has been committed.
#[derive(Default)]
struct TextState {
    serial: Option<u32>,
    content_type: ContentType,
    surrounding: Option<SurroundingText>,
    pending_type: Option<ContentType>,
    pending_surrounding: Option<SurroundingText>,
    reset: bool,
}

impl TextState {
    fn commit(&mut self, serial: u32) -> bool {
        self.serial = Some(serial);
        let reset = std::mem::take(&mut self.reset);
        if reset {
            self.surrounding = None;
        }
        if let Some(kind) = self.pending_type.take() {
            self.content_type = kind;
        }
        if self.content_type.is_sensitive() {
            self.pending_surrounding = None;
            self.surrounding = None;
        } else if let Some(text) = self.pending_surrounding.take() {
            self.surrounding = valid_surrounding(text);
        }
        reset
    }
}

struct PendingKey {
    key: KeyRef,
    /// None for a locally generated repeat, which must use `keysym`, not `key`.
    serial: Option<u32>,
    modifier_serial: Option<u32>,
}

struct Repeat {
    code: u32,
    next: Instant,
    interval: Duration,
}

pub struct Session {
    context: Option<Rc<ZwpInputMethodContextV1>>,
    generation: u32,
    queue: EventQueue,
    outputs: Outputs,
    text: TextState,
    ctx: Option<ContextId>,
    app_id: Option<String>,
    grab: Option<Rc<WlKeyboard>>,
    keyboard: Keyboard,
    keys: KeyTracker,
    held: BTreeSet<u32>,
    pending_keys: VecDeque<PendingKey>,
    forwarded_reply: Option<KeyRef>,
    preedit: Preedit,
    repeat: Option<Repeat>,
    compositor_repeats: bool,
    modifier_serial: Option<u32>,
}

impl Session {
    pub fn new(queue: EventQueue, outputs: Outputs) -> Self {
        Self {
            context: None,
            generation: 0,
            queue,
            outputs,
            text: TextState::default(),
            ctx: None,
            app_id: None,
            grab: None,
            keyboard: Keyboard::new(),
            keys: KeyTracker::default(),
            held: BTreeSet::new(),
            pending_keys: VecDeque::new(),
            forwarded_reply: None,
            preedit: Preedit::default(),
            repeat: None,
            compositor_repeats: false,
            modifier_serial: None,
        }
    }

    pub fn context(&self) -> Option<ContextId> {
        self.ctx
    }

    pub fn set_app_id(&mut self, app_id: Option<String>) {
        if self.app_id != app_id {
            self.app_id = app_id;
            if let Some(ctx) = self.ctx {
                self.outputs.borrow_mut().push_back(FrontendEvent::Updated {
                    ctx,
                    info: self.info(),
                    reset: false,
                });
            }
        }
    }

    fn info(&self) -> ContextInfo {
        ContextInfo {
            app_id: self.app_id.clone(),
            content_type: self.text.content_type,
            surrounding: self
                .text
                .surrounding
                .as_ref()
                .map(|s| (s.text.clone(), s.cursor, s.anchor)),
            cursor_rect: None,
            frontend: Some("wayland".into()),
            client_preedit: true,
        }
    }

    pub fn handle(&mut self, event: V1Event, ids: &ContextIds) -> Option<KeyboardChange> {
        match event {
            V1Event::Activate(context) => {
                self.shutdown();
                self.generation = self.generation.wrapping_add(1);
                context.set_forward_to_client(false);
                context.set_handler(ContextEvents {
                    generation: self.generation,
                    queue: self.queue.clone(),
                });
                // The modifier names and ordering match the engine protocol's X11 bits.
                context.send_modifiers_map(b"Shift\0Lock\0Control\0Mod1\0Mod2\0Mod3\0Mod4\0Mod5\0");
                self.context = Some(context);
            }
            V1Event::Deactivate(context) => {
                if self
                    .context
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &context))
                {
                    self.shutdown();
                }
            }
            V1Event::Context { generation, event }
                if generation == self.generation && self.context.is_some() =>
            {
                match event {
                    ContextEvent::Surrounding(text) => {
                        // Bound even uncommitted input, and never retain a known secret.
                        if !self
                            .text
                            .pending_type
                            .unwrap_or(self.text.content_type)
                            .is_sensitive()
                        {
                            self.text.pending_surrounding = valid_surrounding(text);
                        }
                    }
                    ContextEvent::ContentType(kind) => {
                        self.text.pending_type = Some(kind);
                        if kind.is_sensitive() {
                            self.text.pending_surrounding = None;
                        }
                    }
                    ContextEvent::Reset => {
                        self.text.reset = true;
                        self.text.pending_surrounding = None;
                    }
                    ContextEvent::Commit(serial) => self.commit_state(serial, ids),
                }
            }
            V1Event::Keyboard { generation, event }
                if generation == self.generation && self.grab.is_some() =>
            {
                return self.handle_keyboard(event);
            }
            _ => {}
        }
        None
    }

    fn commit_state(&mut self, serial: u32, ids: &ContextIds) {
        let reset = self.text.commit(serial);
        if self.text.content_type.is_sensitive() {
            // A keyboard created by the v1 context has version 1 and therefore no
            // wl_keyboard.release request. Destroy the context to release its grab
            // when an already focused field becomes sensitive. A fresh activation
            // creates the next context; old queued events are ignored meanwhile.
            if self.grab.is_some() {
                self.shutdown();
            } else {
                self.end_context();
            }
            return;
        }
        if reset {
            self.preedit = Preedit::default();
            self.repeat = None;
        }
        if let Some(ctx) = self.ctx {
            self.outputs.borrow_mut().push_back(FrontendEvent::Updated {
                ctx,
                info: self.info(),
                reset,
            });
        } else {
            let ctx = ids.next();
            self.ctx = Some(ctx);
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::Activated {
                    origin: Origin::Wayland(SEAT),
                    ctx,
                    info: self.info(),
                });
        }
        if self.grab.is_none()
            && let Some(context) = &self.context
        {
            let grab = context.new_send_grab_keyboard();
            grab.set_forward_to_client(false);
            grab.set_handler(KeyboardEvents {
                generation: self.generation,
                queue: self.queue.clone(),
            });
            self.grab = Some(grab);
            debug!("input-method-v1 keyboard grabbed");
        }
    }

    fn handle_keyboard(&mut self, event: KeyboardEvent) -> Option<KeyboardChange> {
        match event {
            KeyboardEvent::Keymap { format, fd, size } => {
                if format == 1 {
                    match keyboard::read_keymap(&fd, size)
                        .map_err(keyboard::KeymapError::from)
                        .and_then(|text| self.keyboard.set_keymap(text))
                    {
                        Ok(true) => {
                            return self
                                .keyboard
                                .keymap_text()
                                .map(|text| KeyboardChange::Keymap(Rc::from(text)));
                        }
                        Ok(false) => {}
                        Err(error) => warn!("cannot use input-method-v1 keymap: {error}"),
                    }
                }
            }
            KeyboardEvent::Modifiers { serial, state } => {
                self.keyboard.set_modifiers(state);
                self.modifier_serial = Some(serial);
                if let Some(context) = &self.context {
                    context.send_modifiers(
                        serial,
                        state.depressed,
                        state.latched,
                        state.locked,
                        state.group,
                    );
                }
                return Some(KeyboardChange::Modifiers(state));
            }
            KeyboardEvent::Repeat(info) => {
                self.keyboard.set_repeat_info(info);
                return Some(KeyboardChange::Repeat(info));
            }
            KeyboardEvent::Key {
                serial,
                time,
                code,
                state,
            } => {
                match state {
                    0 => {
                        self.held.remove(&code);
                        if self.repeat.as_ref().is_some_and(|r| r.code == code) {
                            self.repeat = None;
                        }
                    }
                    1 => {
                        self.held.insert(code);
                        if self.repeat.as_ref().is_some_and(|r| r.code != code) {
                            self.repeat = None;
                        }
                    }
                    2 => {
                        self.compositor_repeats = true;
                        self.repeat = None;
                        if self.keys.is_forwarded(code) {
                            return None;
                        }
                    }
                    _ => return None,
                }
                self.emit_key(time, code, state == 0, state == 2, Some(serial));
            }
        }
        None
    }

    fn emit_key(
        &mut self,
        time: u32,
        code: u32,
        released: bool,
        repeat: bool,
        serial: Option<u32>,
    ) {
        let key = KeyRef {
            seat: SEAT,
            ctx: self.ctx.unwrap_or(0),
            keycode: code,
            time,
            released,
            repeat,
            modifiers: self.keyboard.modifiers(),
        };
        // The core has its own bounded queue; fail open before growing ours beyond it.
        if key.ctx == 0 || self.pending_keys.len() >= MAX_PENDING_KEYS {
            if let (Some(context), Some(serial)) = (&self.context, serial) {
                context.send_key(serial, time, code, u32::from(!released));
            }
            if released {
                self.keys.release(code);
            } else {
                self.keys.press_forwarded(code);
            }
            return;
        }
        let decoded = self.keyboard.decode(code);
        let event = KeyEvent {
            keysym: decoded.as_ref().map_or(0, |d| d.keysym),
            keycode: code,
            modifiers: self.keyboard.modifier_bits(),
            time,
            text: decoded.map(|d| d.utf8).unwrap_or_default(),
            released,
            repeat,
        };
        self.pending_keys.push_back(PendingKey {
            key,
            serial,
            modifier_serial: self.modifier_serial,
        });
        self.outputs.borrow_mut().push_back(FrontendEvent::Key {
            key: frontend::KeyRef::Wayland(key),
            event,
        });
    }

    fn pending_serials(&self, key: &KeyRef) -> (Option<u32>, Option<u32>) {
        self.pending_keys
            .iter()
            .find(|pending| pending.key == *key)
            .map_or((None, None), |pending| {
                (pending.serial, pending.modifier_serial)
            })
    }

    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        let pending = self
            .pending_keys
            .iter()
            .position(|pending| pending.key == *key)
            .and_then(|index| self.pending_keys.remove(index));
        let Some(pending) = pending else {
            return;
        };
        if Some(key.ctx) != self.ctx {
            return;
        }
        if self.forwarded_reply.take() == Some(*key) {
            return;
        }
        if key.released {
            if self.keys.release(key.keycode) == ReleaseAction::Forward {
                self.send_key(key, pending.serial, pending.modifier_serial);
            }
        } else if handled {
            if !key.repeat {
                self.keys.press_consumed(key.keycode);
                self.start_repeat(key.keycode);
            }
        } else {
            self.send_key(key, pending.serial, pending.modifier_serial);
            if !key.repeat {
                self.keys.press_forwarded(key.keycode);
            }
        }
    }

    pub fn finish_delegated(&mut self, key: &KeyRef) {
        self.finish_key(key, true);
    }

    fn send_key(&mut self, key: &KeyRef, serial: Option<u32>, modifier_serial: Option<u32>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        if let Some(serial) = serial {
            let current = self.keyboard.modifiers();
            if key.modifiers != current
                && let Some(modifier_serial) = modifier_serial
            {
                context.send_modifiers(
                    modifier_serial,
                    key.modifiers.depressed,
                    key.modifiers.latched,
                    key.modifiers.locked,
                    key.modifiers.group,
                );
            }
            let state = if key.released {
                0
            } else if key.repeat {
                2
            } else {
                1
            };
            context.send_key(serial, key.time, key.keycode, state);
            if key.modifiers != current
                && let Some(modifier_serial) = self.modifier_serial
            {
                context.send_modifiers(
                    modifier_serial,
                    current.depressed,
                    current.latched,
                    current.locked,
                    current.group,
                );
            }
        } else {
            self.send_keysym(key.keycode, !key.released, key.time, key.modifiers);
            if key.repeat {
                self.send_keysym(key.keycode, false, key.time, key.modifiers);
            }
        }
    }

    fn send_keysym(&mut self, code: u32, pressed: bool, time: u32, modifiers: ModifierState) {
        let current = self.keyboard.modifiers();
        self.keyboard.set_modifiers(modifiers);
        let decoded = self.keyboard.decode(code);
        let bits = self.keyboard.modifier_bits();
        self.keyboard.set_modifiers(current);
        if let (Some(context), Some(serial), Some(decoded)) =
            (&self.context, self.text.serial, decoded)
        {
            context.send_keysym(serial, time, decoded.keysym, u32::from(pressed), bits);
        }
    }

    fn start_repeat(&mut self, code: u32) {
        let info = self.keyboard.repeat_info();
        if !self.compositor_repeats
            && self.held.contains(&code)
            && info.rate > 0
            && self.keyboard.decode(code).is_some_and(|d| d.repeats)
        {
            self.repeat = Some(Repeat {
                code,
                next: Instant::now() + Duration::from_millis(info.delay.max(0) as u64),
                interval: Duration::from_secs(1) / (info.rate as u32).min(1000),
            });
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.repeat.as_ref().map(|r| r.next)
    }

    pub fn on_timer(&mut self, now: Instant) {
        if let Some(repeat) = &mut self.repeat
            && repeat.next <= now
        {
            repeat.next = now + repeat.interval;
            let code = repeat.code;
            self.emit_key(super::seat::now_ms(), code, false, true, None);
        }
    }

    pub fn apply_actions(
        &mut self,
        ctx: ContextId,
        actions: &[Action],
        reply_key: Option<&KeyRef>,
    ) {
        if Some(ctx) != self.ctx {
            return;
        }
        let (Some(context), Some(serial)) = (self.context.clone(), self.text.serial) else {
            return;
        };
        for action in actions {
            match action {
                Action::InputMode { .. } => {}
                Action::Commit(text) => {
                    self.preedit = Preedit::default();
                    if text.is_empty() {
                        context.send_commit_string(serial, "");
                    } else {
                        for chunk in text_chunks(text) {
                            context.send_commit_string(serial, chunk);
                        }
                    }
                }
                Action::Preedit(preedit) => {
                    self.preedit = bounded_preedit(preedit);
                    context.send_preedit_cursor(self.preedit.cursor_begin);
                    if !self.preedit.text.is_empty() {
                        context.send_preedit_styling(0, self.preedit.text.len() as u32, 1);
                    }
                    context.send_preedit_string(serial, &self.preedit.text, "");
                }
                Action::DeleteSurrounding { before, after } => {
                    if let Some((index, length)) =
                        deletion(self.text.surrounding.as_ref(), *before, *after)
                    {
                        context.send_delete_surrounding_text(index, length);
                        // V1 applies deletion on commit_string/preedit_string.
                        context.send_commit_string(serial, "");
                    }
                }
                Action::ForwardKey { keycode, pressed } => {
                    if let Some(key) =
                        reply_key.filter(|key| key.keycode == *keycode && key.released != *pressed)
                    {
                        let (serial, modifier_serial) = self.pending_serials(key);
                        self.send_key(key, serial, modifier_serial);
                        self.forwarded_reply = Some(*key);
                    } else {
                        self.send_keysym(
                            *keycode,
                            *pressed,
                            super::seat::now_ms(),
                            self.keyboard.modifiers(),
                        );
                    }
                    if *pressed {
                        if self.held.contains(keycode) {
                            self.keys.press_forwarded(*keycode);
                        } else {
                            // The physical release may already have been handled
                            // while a hosted engine was deciding about the press.
                            self.send_keysym(
                                *keycode,
                                false,
                                super::seat::now_ms(),
                                self.keyboard.modifiers(),
                            );
                            self.keys.press_consumed(*keycode);
                        }
                        self.repeat = None;
                    } else {
                        self.keys.release(*keycode);
                        if reply_key.is_some_and(|key| !key.released) {
                            self.keys.press_consumed(*keycode);
                        }
                    }
                }
            }
        }
    }

    pub fn forward_key(&mut self, ctx: ContextId, key: &frontend::ForwardedKey) {
        if Some(ctx) != self.ctx {
            return;
        }
        self.send_keysym(key.keycode, key.pressed, key.time, key.modifiers);
        if key.pressed {
            if self.held.contains(&key.keycode) {
                self.keys.press_forwarded(key.keycode);
            } else {
                self.send_keysym(key.keycode, false, key.time, key.modifiers);
                self.keys.press_consumed(key.keycode);
            }
        } else {
            self.keys.release(key.keycode);
        }
    }

    pub fn clear_preedit(&mut self, ctx: ContextId) {
        if Some(ctx) == self.ctx && !self.preedit.text.is_empty() {
            self.preedit = Preedit::default();
            if let (Some(context), Some(serial)) = (&self.context, self.text.serial) {
                context.send_preedit_cursor(0);
                context.send_preedit_string(serial, "", "");
            }
        }
    }

    fn end_context(&mut self) {
        if let Some(ctx) = self.ctx.take() {
            self.outputs
                .borrow_mut()
                .push_back(FrontendEvent::Deactivated { ctx });
        }
        self.preedit = Preedit::default();
        self.pending_keys.clear();
        self.forwarded_reply = None;
        self.repeat = None;
    }

    fn release_grab(&mut self) {
        if let Some(grab) = self.grab.take() {
            // A v1 keyboard inherits the v1 context version, so it has no release
            // request. Destroying the context below releases the server-side grab.
            grab.unset_handler();
            if grab.version() >= 3 {
                grab.send_release();
            }
        }
        self.held.clear();
        self.keys.take_forwarded();
        self.repeat = None;
    }

    pub fn shutdown(&mut self) {
        self.end_context();
        self.release_grab();
        if let Some(context) = self.context.take() {
            context.unset_handler();
            context.send_destroy();
        }
        self.text = TextState::default();
        self.keyboard = Keyboard::new();
        self.compositor_repeats = false;
        self.modifier_serial = None;
    }
}

fn content_type(hints: u32, purpose: u32) -> ContentType {
    // V1 predates PIN, so its date/time/terminal values are one lower than V3.
    let purpose = match purpose {
        0..=8 => ContentPurpose::from_raw(purpose),
        9 => ContentPurpose::Date,
        10 => ContentPurpose::Time,
        11 => ContentPurpose::DateTime,
        12 => ContentPurpose::Terminal,
        raw => ContentPurpose::Unknown(raw),
    };
    ContentType {
        hints: ContentHints(hints),
        purpose,
    }
}

fn valid_surrounding(text: SurroundingText) -> Option<SurroundingText> {
    (text.text.len() <= MAX_TEXT_BYTES
        && text.text.is_char_boundary(text.cursor as usize)
        && text.text.is_char_boundary(text.anchor as usize))
    .then_some(text)
}

fn deletion(text: Option<&SurroundingText>, before: u32, after: u32) -> Option<(i32, u32)> {
    let text = text?;
    let start = text.cursor.checked_sub(before)?;
    let end = text.cursor.checked_add(after)?;
    if !text.text.is_char_boundary(start as usize) || !text.text.is_char_boundary(end as usize) {
        return None;
    }
    Some((-i32::try_from(before).ok()?, before.checked_add(after)?))
}

fn text_chunks(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let mut end = text.len().min(MAX_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = text.split_at(end);
        text = rest;
        Some(chunk)
    })
}

fn bounded_preedit(preedit: &Preedit) -> Preedit {
    let text = text_chunks(&preedit.text).next().unwrap_or("").to_owned();
    let cursor = |position: i32| {
        if position < 0 {
            return -1;
        }
        let mut position = (position as usize).min(text.len());
        while !text.is_char_boundary(position) {
            position -= 1;
        }
        position as i32
    };
    Preedit {
        cursor_begin: cursor(preedit.cursor_begin),
        cursor_end: cursor(preedit.cursor_end),
        text,
    }
}

pub struct MethodEvents {
    pub queue: EventQueue,
}
impl ZwpInputMethodV1Handler for MethodEvents {
    fn handle_activate(&mut self, _: &Rc<ZwpInputMethodV1>, context: &Rc<ZwpInputMethodContextV1>) {
        // Install handlers immediately: the following context events can arrive in
        // the same wl-proxy dispatch as activate, before the queue is drained.
        context.set_forward_to_client(false);
        context.set_handler(EarlyContextEvents {
            queue: self.queue.clone(),
        });
        self.queue
            .borrow_mut()
            .push_back(Event::V1(V1Event::Activate(context.clone())));
    }
    fn handle_deactivate(
        &mut self,
        _: &Rc<ZwpInputMethodV1>,
        context: &Rc<ZwpInputMethodContextV1>,
    ) {
        self.queue
            .borrow_mut()
            .push_back(Event::V1(V1Event::Deactivate(context.clone())));
    }
}

// Activate has not yet assigned the session generation. Carry the context identity
// until the event queue reaches it; the frontend resolves it then.
struct EarlyContextEvents {
    queue: EventQueue,
}
impl EarlyContextEvents {
    fn push(&self, context: &Rc<ZwpInputMethodContextV1>, event: ContextEvent) {
        self.queue.borrow_mut().push_back(Event::V1Context {
            context: context.clone(),
            event,
        });
    }
}
impl ZwpInputMethodContextV1Handler for EarlyContextEvents {
    fn handle_surrounding_text(
        &mut self,
        context: &Rc<ZwpInputMethodContextV1>,
        text: &str,
        cursor: u32,
        anchor: u32,
    ) {
        self.push(
            context,
            ContextEvent::Surrounding(SurroundingText {
                text: text.into(),
                cursor,
                anchor,
            }),
        );
    }
    fn handle_content_type(
        &mut self,
        context: &Rc<ZwpInputMethodContextV1>,
        hint: u32,
        purpose: u32,
    ) {
        self.push(
            context,
            ContextEvent::ContentType(content_type(hint, purpose)),
        );
    }
    fn handle_reset(&mut self, context: &Rc<ZwpInputMethodContextV1>) {
        self.push(context, ContextEvent::Reset);
    }
    fn handle_commit_state(&mut self, context: &Rc<ZwpInputMethodContextV1>, serial: u32) {
        self.push(context, ContextEvent::Commit(serial));
    }
}

impl Session {
    pub fn early_context(
        &mut self,
        context: &Rc<ZwpInputMethodContextV1>,
        event: ContextEvent,
        ids: &ContextIds,
    ) {
        if self
            .context
            .as_ref()
            .is_some_and(|active| Rc::ptr_eq(active, context))
        {
            self.handle(
                V1Event::Context {
                    generation: self.generation,
                    event,
                },
                ids,
            );
        }
    }
}

struct ContextEvents {
    generation: u32,
    queue: EventQueue,
}
impl ContextEvents {
    fn push(&self, event: ContextEvent) {
        self.queue
            .borrow_mut()
            .push_back(Event::V1(V1Event::Context {
                generation: self.generation,
                event,
            }));
    }
}
impl ZwpInputMethodContextV1Handler for ContextEvents {
    fn handle_surrounding_text(
        &mut self,
        _: &Rc<ZwpInputMethodContextV1>,
        text: &str,
        cursor: u32,
        anchor: u32,
    ) {
        self.push(ContextEvent::Surrounding(SurroundingText {
            text: text.into(),
            cursor,
            anchor,
        }));
    }
    fn handle_content_type(&mut self, _: &Rc<ZwpInputMethodContextV1>, hint: u32, purpose: u32) {
        self.push(ContextEvent::ContentType(content_type(hint, purpose)));
    }
    fn handle_reset(&mut self, _: &Rc<ZwpInputMethodContextV1>) {
        self.push(ContextEvent::Reset);
    }
    fn handle_commit_state(&mut self, _: &Rc<ZwpInputMethodContextV1>, serial: u32) {
        self.push(ContextEvent::Commit(serial));
    }
}

struct KeyboardEvents {
    generation: u32,
    queue: EventQueue,
}
impl KeyboardEvents {
    fn push(&self, event: KeyboardEvent) {
        self.queue
            .borrow_mut()
            .push_back(Event::V1(V1Event::Keyboard {
                generation: self.generation,
                event,
            }));
    }
}
impl WlKeyboardHandler for KeyboardEvents {
    fn handle_keymap(
        &mut self,
        _: &Rc<WlKeyboard>,
        format: WlKeyboardKeymapFormat,
        fd: &Rc<OwnedFd>,
        size: u32,
    ) {
        self.push(KeyboardEvent::Keymap {
            format: format.0,
            fd: fd.clone(),
            size,
        });
    }
    fn handle_key(
        &mut self,
        _: &Rc<WlKeyboard>,
        serial: u32,
        time: u32,
        key: u32,
        state: WlKeyboardKeyState,
    ) {
        self.push(KeyboardEvent::Key {
            serial,
            time,
            code: key,
            state: state.0,
        });
    }
    fn handle_modifiers(
        &mut self,
        _: &Rc<WlKeyboard>,
        serial: u32,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    ) {
        self.push(KeyboardEvent::Modifiers {
            serial,
            state: ModifierState {
                depressed,
                latched,
                locked,
                group,
            },
        });
    }
    fn handle_repeat_info(&mut self, _: &Rc<WlKeyboard>, rate: i32, delay: i32) {
        self.push(KeyboardEvent::Repeat(RepeatInfo { rate, delay }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use wl_proxy::baseline::Baseline;
    use wl_proxy::protocols::wayland::wl_registry::WlRegistry;
    use wl_proxy::state::State;

    struct Wire {
        state: Rc<State>,
        peer: UnixStream,
        context: Rc<ZwpInputMethodContextV1>,
    }

    impl Wire {
        fn new() -> Self {
            let (client, peer) = UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            let state = State::builder(Baseline::V6)
                .with_server_fd(&Rc::new(client.into()))
                .build()
                .unwrap();
            let registry: Rc<WlRegistry> = state.display().new_send_get_registry();
            registry.set_forward_to_client(false);
            let context = state.create_object::<ZwpInputMethodContextV1>(1);
            // A synthetic registry bind assigns an upstream ID without needing a
            // real compositor. All requests below use the normal wire serializer.
            registry.send_bind(100, context.clone());
            Self {
                state,
                peer,
                context,
            }
        }

        fn messages(&mut self) -> Vec<(u32, u16, Vec<u8>)> {
            self.state.before_poll().unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match self.peer.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => bytes.extend_from_slice(&buffer[..size]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("wire read: {error}"),
                }
            }
            let mut messages = Vec::new();
            while !bytes.is_empty() {
                let id = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
                let header = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
                let size = (header >> 16) as usize;
                assert!(size >= 8 && size <= bytes.len());
                messages.push((id, header as u16, bytes[8..size].to_vec()));
                bytes.drain(..size);
            }
            messages
        }

        fn context_requests(&mut self) -> Vec<(u16, Vec<u8>)> {
            let id = self.context.server_id().unwrap();
            self.messages()
                .into_iter()
                .filter_map(|(object, opcode, body)| (object == id).then_some((opcode, body)))
                .collect()
        }
    }

    fn words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_ne_bytes(*word))
            .collect()
    }

    fn pop_key(outputs: &Outputs) -> KeyRef {
        outputs
            .borrow_mut()
            .drain(..)
            .find_map(|event| match event {
                FrontendEvent::Key {
                    key: frontend::KeyRef::Wayland(key),
                    ..
                } => Some(key),
                _ => None,
            })
            .expect("queued key")
    }

    #[test]
    fn forwarded_keyboard_events_echo_their_own_serial_not_text_serial() {
        let mut wire = Wire::new();
        let outputs = Outputs::default();
        let mut session = Session::new(EventQueue::default(), outputs.clone());
        let ids = ContextIds::default();
        session.handle(V1Event::Activate(wire.context.clone()), &ids);
        session.commit_state(41, &ids);
        wire.messages();

        session.handle_keyboard(KeyboardEvent::Key {
            serial: 901,
            time: 1234,
            code: 30,
            state: 1,
        });
        let press = pop_key(&outputs);
        // Application state may advance while an asynchronous engine decides.
        session.commit_state(42, &ids);
        session.finish_key(&press, false);
        session.handle_keyboard(KeyboardEvent::Key {
            serial: 907,
            time: 1239,
            code: 30,
            state: 0,
        });
        let release = pop_key(&outputs);
        session.finish_key(&release, true);
        let keys: Vec<_> = wire
            .context_requests()
            .into_iter()
            .filter_map(|(opcode, body)| (opcode == 10).then(|| words(&body)))
            .collect();
        assert_eq!(keys, vec![vec![901, 1234, 30, 1], vec![907, 1239, 30, 0]]);
    }

    #[test]
    fn password_activation_never_grabs_or_creates_an_engine_context() {
        let mut wire = Wire::new();
        let outputs = Outputs::default();
        let mut session = Session::new(EventQueue::default(), outputs.clone());
        let ids = ContextIds::default();
        session.handle(V1Event::Activate(wire.context.clone()), &ids);
        session.text.pending_type = Some(content_type(0, 8));
        session.text.pending_surrounding = Some(SurroundingText {
            text: "secret".into(),
            cursor: 6,
            anchor: 6,
        });
        session.commit_state(1, &ids);
        assert!(session.context().is_none());
        assert!(session.grab.is_none());
        assert!(session.text.surrounding.is_none());
        assert!(outputs.borrow().is_empty());
        assert!(
            wire.context_requests()
                .iter()
                .all(|(opcode, _)| *opcode != 9)
        );
    }

    #[test]
    fn delayed_raw_key_restores_its_modifier_snapshot_then_current_state() {
        let mut wire = Wire::new();
        let outputs = Outputs::default();
        let mut session = Session::new(EventQueue::default(), outputs.clone());
        let ids = ContextIds::default();
        session.handle(V1Event::Activate(wire.context.clone()), &ids);
        session.commit_state(12, &ids);
        session.handle_keyboard(KeyboardEvent::Modifiers {
            serial: 501,
            state: ModifierState {
                depressed: 1,
                ..Default::default()
            },
        });
        session.handle_keyboard(KeyboardEvent::Key {
            serial: 502,
            time: 300,
            code: 30,
            state: 1,
        });
        let key = pop_key(&outputs);
        session.handle_keyboard(KeyboardEvent::Modifiers {
            serial: 503,
            state: ModifierState::default(),
        });
        wire.messages();
        session.finish_key(&key, false);
        let requests: Vec<_> = wire
            .context_requests()
            .into_iter()
            .map(|(opcode, body)| (opcode, words(&body)))
            .collect();
        assert_eq!(
            requests,
            [
                (11, vec![501, 1, 0, 0, 0]),
                (10, vec![502, 300, 30, 1]),
                (11, vec![503, 0, 0, 0, 0]),
            ]
        );
        // A duplicate reply must not replay a key after its serial was retired.
        session.finish_key(&key, false);
        assert!(wire.context_requests().is_empty());
    }

    #[test]
    fn sensitive_transition_releases_the_context_and_drops_late_actions() {
        let mut wire = Wire::new();
        let outputs = Outputs::default();
        let mut session = Session::new(EventQueue::default(), outputs.clone());
        let ids = ContextIds::default();
        session.handle(V1Event::Activate(wire.context.clone()), &ids);
        session.commit_state(4, &ids);
        let ctx = session.context().unwrap();
        session.handle_keyboard(KeyboardEvent::Key {
            serial: 8,
            time: 1,
            code: 30,
            state: 1,
        });
        let key = pop_key(&outputs);
        let context_id = wire.context.server_id().unwrap();
        wire.messages();
        session.text.pending_type = Some(content_type(0x80, 0));
        session.commit_state(5, &ids);
        session.apply_actions(ctx, &[Action::Commit("late".into())], Some(&key));
        session.finish_key(&key, false);
        assert!(session.context.is_none() && session.grab.is_none());
        assert!(session.pending_keys.is_empty());
        assert!(outputs.borrow().iter().any(
            |event| matches!(event, FrontendEvent::Deactivated { ctx: ended } if *ended == ctx)
        ));
        let requests: Vec<_> = wire
            .messages()
            .into_iter()
            .filter(|(id, _, _)| *id == context_id)
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, 0); // context.destroy, no late commit/key
    }

    #[test]
    fn v1_purpose_values_do_not_inherit_v3_pin_offset() {
        assert!(content_type(0, 8).is_sensitive());
        assert_eq!(content_type(0, 9).purpose, ContentPurpose::Date);
        assert_eq!(content_type(0, 12).purpose, ContentPurpose::Terminal);
        assert!(content_type(0x80, 0).is_sensitive());
    }

    #[test]
    fn state_is_staged_and_sensitive_text_is_erased() {
        let mut text = TextState::default();
        text.pending_surrounding = Some(SurroundingText {
            text: "abc".into(),
            cursor: 3,
            anchor: 3,
        });
        assert!(text.serial.is_none() && text.surrounding.is_none());
        assert!(!text.commit(87));
        assert_eq!(text.serial, Some(87));
        assert!(text.surrounding.is_some());
        text.pending_type = Some(content_type(0, 8));
        text.commit(103);
        assert!(text.surrounding.is_none() && text.content_type.is_sensitive());
        text.pending_type = Some(ContentType::default());
        text.reset = true;
        assert!(text.commit(104));
        assert!(text.surrounding.is_none());
    }

    #[test]
    fn deletion_requires_valid_utf8_boundaries_and_available_context() {
        let text = SurroundingText {
            text: "a中文b".into(),
            cursor: 4,
            anchor: 4,
        };
        assert_eq!(deletion(Some(&text), 3, 3), Some((-3, 6)));
        assert_eq!(deletion(Some(&text), 2, 0), None);
        assert_eq!(deletion(Some(&text), 5, 0), None);
        assert_eq!(deletion(None, 1, 0), None);
    }

    #[test]
    fn text_and_preedit_bounds_preserve_utf8() {
        let text = "中".repeat(3000);
        let chunks: Vec<_> = text_chunks(&text).collect();
        assert!(chunks.iter().all(|s| s.len() <= MAX_TEXT_BYTES));
        assert_eq!(chunks.concat(), text);
        let preedit = bounded_preedit(&Preedit {
            text,
            cursor_begin: 4000,
            cursor_end: -1,
        });
        assert_eq!(preedit.text.len(), 3999);
        assert_eq!(preedit.cursor_begin, 3999);
        assert_eq!(preedit.cursor_end, -1);
    }
}
