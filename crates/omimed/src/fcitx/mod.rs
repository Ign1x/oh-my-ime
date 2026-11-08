//! The Fcitx5 input method module protocol, for applications that load the Fcitx5 GTK or
//! Qt module (`GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`). It matters most for X11
//! applications under XWayland, which the compositor's text input protocol does not
//! reach.
//!
//! omimed serves the protocol under the names fcitx5 would own on the session bus,
//! `org.fcitx.Fcitx5` and `org.freedesktop.portal.Fcitx` (which Flatpak applications may
//! talk to), and queues for them while another program holds them.

mod convert;
mod output;
mod service;
mod window;

use std::collections::{BTreeMap, VecDeque};

use omime_proto::engine::{Action, ContextId, ContextInfo};
#[cfg(test)]
use tokio::sync::oneshot;
use tracing::debug;
use xkbcommon::xkb;
use zbus::OwnedObjectPath;

use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
use crate::placement::{self, Rect};
pub use output::{KeyReply, OutputQueue};
pub use service::start;

/// Fcitx frontend resource limits, shared by both bus names.
const MAX_CONTEXTS_PER_OWNER: usize = 1_024;
const MAX_CONTEXTS: usize = 8_192;
const MAX_SURROUNDING_BYTES: usize = 4_000;
const MAX_PENDING_KEYS: usize = 1_024;

/// Identifies an input context of the protocol.
pub type IcId = u64;

/// The two bus connections, by the name they own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// `org.fcitx.Fcitx5`
    Main = 0,
    /// `org.freedesktop.portal.Fcitx`
    Portal = 1,
}

/// A key from an application, waiting for the engine's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRef {
    pub ctx: ContextId,
    id: u64,
}

/// What the D-Bus objects ask of the frontend.
#[derive(Debug)]
pub enum Request {
    Created {
        ic: IcId,
        bus: Bus,
        owner: String,
        path: OwnedObjectPath,
        args: Vec<(String, String)>,
    },
    FocusIn(IcId),
    FocusOut(IcId),
    Reset(IcId),
    /// The text cursor; in X screen coordinates for X11 applications, relative to the
    /// window for Wayland ones. `scale` divides it into logical pixels on Wayland.
    CursorRect {
        ic: IcId,
        rect: Rect,
        scale: f64,
    },
    /// Where the focused window is on the screen, as the compositor says, after a
    /// Wayland application focused a context.
    Window {
        ic: IcId,
        window: Option<Rect>,
    },
    Capability {
        ic: IcId,
        capability: u64,
    },
    /// Text around the cursor, with the cursor and anchor in characters. Without
    /// `text`, only the cursor moved.
    Surrounding {
        ic: IcId,
        text: Option<String>,
        cursor: u32,
        anchor: u32,
    },
    Destroy(IcId),
    /// `ProcessKeyEvent` (`batch` false) or `ProcessKeyEventBatch`: keyval, key code,
    /// state, release, time.
    Key {
        ic: IcId,
        key: (u32, u32, u32, bool, u32),
        batch: bool,
        reply: KeyReply,
    },
    /// An application left the bus.
    ClientGone(String),
    /// Its output could not be delivered within the transport budget.
    OutputFailed {
        ic: IcId,
        ctx: Option<ContextId>,
    },
}

/// A signal of `org.fcitx.Fcitx.InputContext1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Commit(String),
    Preedit(Vec<(String, i32)>, i32),
    /// keyval, state, release
    Forward(u32, u32, bool),
    /// offset from the cursor and length, in characters
    Delete(i32, u32),
}

/// A signal returned in the reply of `ProcessKeyEventBatch` instead of being sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batched(pub Signal);

/// The answer to a key.
#[derive(Debug, Default)]
pub struct KeyAnswer {
    pub handled: bool,
    /// What the key produced, for `ProcessKeyEventBatch`.
    pub batch: Vec<Batched>,
}

/// What the frontend asks a bus connection to do, in order.
#[derive(Debug)]
pub enum Outgoing {
    Signal {
        dest: String,
        path: OwnedObjectPath,
        signal: Signal,
    },
    /// Answers a key; everything queued before has been sent by then.
    Reply { reply: KeyReply, answer: KeyAnswer },
    /// Removes an input context's object from the bus.
    Remove(OwnedObjectPath),
}

#[derive(Debug)]
struct Ic {
    bus: Bus,
    owner: String,
    path: OwnedObjectPath,
    program: Option<String>,
    /// Whether the application runs on X11 (`display` is `x11:`).
    x11: bool,
    /// Whether the application runs on Wayland (`display` is `wayland:`).
    wayland: bool,
    /// The text cursor: in X screen coordinates for X11 applications, relative to the
    /// window for Wayland ones.
    cursor_rect: Option<Rect>,
    /// What divides `cursor_rect` into logical pixels, for Wayland applications.
    cursor_scale: f64,
    /// Where the focused window was when the application last focused this field.
    window: Option<Rect>,
    capability: u64,
    focused: bool,
    /// Output retirement is distinct from temporarily sensitive fields. Only a
    /// genuine focus transition may permit composition again.
    output_retired: bool,
    /// The core's context while the field has focus and is not sensitive.
    ctx: Option<ContextId>,
    surrounding: Option<Surrounding>,
    /// Keep an explicit empty buffer in every update of this live context until
    /// fresh text arrives, including when hosted delivery is waiting for a grab.
    surrounding_cleared: bool,
    preedit_shown: bool,
    /// The modifiers of the last key, which keys engines send back carry too.
    last_mods: u32,
}

#[derive(Debug, Default)]
struct Surrounding {
    text: String,
    /// Cursor and anchor in characters within the retained window.
    cursor: u32,
    anchor: u32,
    /// Characters discarded before the window; position-only updates use the original text.
    offset: u32,
    /// False after the cursor leaves the window, until the client sends fresh text.
    valid: bool,
}

impl Surrounding {
    fn new(text: String, cursor: u32, anchor: u32) -> Self {
        let cursor_byte = convert::byte_offset(&text, cursor) as usize;
        let mut start = cursor_byte
            .saturating_sub(MAX_SURROUNDING_BYTES / 2)
            .min(text.len().saturating_sub(MAX_SURROUNDING_BYTES));
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let mut end = (start + MAX_SURROUNDING_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let offset = text[..start].chars().count() as u32;
        // Copy the slice so a large input allocation is not retained by the window.
        let text = text[start..end].to_owned();
        let chars = text.chars().count() as u32;
        Self {
            text,
            cursor: cursor.saturating_sub(offset).min(chars),
            anchor: anchor.saturating_sub(offset).min(chars),
            offset,
            valid: true,
        }
    }

    /// A cursor outside the retained text needs a fresh full-text update.
    fn set_position(&mut self, cursor: u32, anchor: u32) {
        let chars = self.text.chars().count() as u32;
        if !self.valid || cursor < self.offset || cursor - self.offset > chars {
            // Absent fields in UpdateContext preserve the engine's old values. Keep
            // an explicit empty buffer so the engine forgets the discarded window.
            *self = Self::default();
            return;
        }
        self.cursor = cursor - self.offset;
        self.anchor = anchor.saturating_sub(self.offset).min(chars);
    }
}

/// A key the engine has not answered yet.
#[derive(Debug)]
struct Pending {
    ic: IcId,
    reply: KeyReply,
    /// keyval, state, release: what goes back to the application in `ForwardKey`.
    key: (u32, u32, bool),
    /// For `ProcessKeyEventBatch`: what the key produced so far.
    batch: Option<Vec<Batched>>,
    /// Whether anything was sent to the application for this key.
    produced: bool,
}

/// The input contexts of applications using the Fcitx5 module.
pub struct FcitxFrontend {
    queues: [Option<OutputQueue>; 2],
    context_ids: ContextIds,
    ics: BTreeMap<IcId, Ic>,
    /// By key id; ids grow, so the first entry of a context is its oldest key.
    pending: BTreeMap<u64, Pending>,
    next_key: u64,
    events: VecDeque<FrontendEvent>,
    /// Turns key codes of `ForwardKey` actions into keysyms.
    keymap: Option<xkb::State>,
}

impl std::fmt::Debug for FcitxFrontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FcitxFrontend")
            .field("ics", &self.ics.len())
            .finish_non_exhaustive()
    }
}

impl Drop for FcitxFrontend {
    fn drop(&mut self) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            let _ = pending.reply.send(KeyAnswer {
                handled: pending.produced,
                batch: Vec::new(),
            });
        }
    }
}

impl FcitxFrontend {
    /// A frontend sending through `queues` (from [`start`]).
    pub fn new(queues: [Option<OutputQueue>; 2], context_ids: ContextIds) -> Self {
        Self {
            queues,
            context_ids,
            ics: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_key: 0,
            events: VecDeque::new(),
            keymap: None,
        }
    }

    /// Takes the events for the core.
    pub fn take_events(&mut self) -> Vec<FrontendEvent> {
        self.events.drain(..).collect()
    }

    /// The most recently focused field of an X11 application and its text cursor, in X
    /// screen coordinates, if the application reported one.
    pub fn focused_x11_cursor(&self) -> Option<(ContextId, Option<Rect>)> {
        self.ics
            .values()
            .filter(|ic| ic.x11)
            .filter_map(|ic| Some((ic.ctx?, ic.cursor_rect)))
            .max_by_key(|(ctx, _)| *ctx)
    }

    /// The most recently focused field of a Wayland application and its text cursor, in
    /// compositor coordinates, if the application reported one and the compositor said
    /// where its window is (see [`crate::compositor_ipc`]).
    pub fn focused_wayland_cursor(&self) -> Option<(ContextId, Option<Rect>)> {
        self.ics
            .values()
            .filter(|ic| ic.wayland)
            .filter_map(|ic| Some((ic.ctx?, on_screen(ic))))
            .max_by_key(|(ctx, _)| *ctx)
    }

    pub fn handle(&mut self, request: Request) {
        match request {
            Request::Created {
                ic,
                bus,
                owner,
                path,
                args,
            } => {
                let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
                debug!(
                    ic,
                    program = arg("program"),
                    display = arg("display"),
                    "input context created"
                );
                self.ics.insert(
                    ic,
                    Ic {
                        bus,
                        owner,
                        path,
                        program: arg("program").filter(|p| !p.is_empty()),
                        x11: arg("display").is_some_and(|d| d.starts_with("x11:")),
                        wayland: arg("display").is_some_and(|d| d.starts_with("wayland:")),
                        cursor_rect: None,
                        cursor_scale: 1.0,
                        window: None,
                        capability: 0,
                        focused: false,
                        output_retired: false,
                        ctx: None,
                        surrounding: None,
                        surrounding_cleared: false,
                        preedit_shown: false,
                        last_mods: 0,
                    },
                );
            }
            Request::FocusIn(ic) => self.focus_in(ic),
            Request::FocusOut(ic) => {
                if let Some(state) = self.ics.get_mut(&ic) {
                    state.focused = false;
                    state.output_retired = false;
                }
                self.end_context(ic);
            }
            Request::Reset(ic) => self.updated(ic, true),
            Request::CursorRect { ic, rect, scale } => {
                if let Some(state) = self.ics.get_mut(&ic) {
                    state.cursor_rect = Some(placement::clamp_rect(rect));
                    state.cursor_scale = cursor_scale(scale);
                }
            }
            Request::Window { ic, window } => {
                if let Some(state) = self.ics.get_mut(&ic) {
                    state.window = window.map(placement::clamp_rect);
                }
            }
            Request::Capability { ic, capability } => {
                let Some(state) = self.ics.get_mut(&ic) else {
                    return;
                };
                let dropped_surrounding = state.capability & convert::capability::SURROUNDING_TEXT
                    != 0
                    && capability & convert::capability::SURROUNDING_TEXT == 0;
                state.capability = capability;
                // Becoming a password field (or no longer) starts or ends the context.
                let sensitive = convert::content_type(capability).is_sensitive();
                if (sensitive || dropped_surrounding) && state.surrounding.take().is_some() {
                    state.surrounding_cleared = state.ctx.is_some();
                }
                if state.focused && sensitive == state.ctx.is_some() {
                    if sensitive {
                        self.end_context(ic);
                    } else if !state.output_retired {
                        self.start_context(ic);
                    }
                } else {
                    self.updated(ic, false);
                }
            }
            Request::Surrounding {
                ic,
                text,
                cursor,
                anchor,
            } => {
                let Some(state) = self.ics.get_mut(&ic) else {
                    return;
                };
                if convert::content_type(state.capability).is_sensitive() {
                    return;
                }
                state.surrounding = match (text, state.surrounding.take()) {
                    (Some(text), _) => {
                        state.surrounding_cleared = false;
                        Some(Surrounding::new(text, cursor, anchor))
                    }
                    (None, Some(mut surrounding)) => {
                        surrounding.set_position(cursor, anchor);
                        Some(surrounding)
                    }
                    (None, None) => return,
                };
                self.updated(ic, false);
            }
            Request::Destroy(ic) => self.destroy(ic),
            Request::Key {
                ic,
                key,
                batch,
                reply,
            } => self.key(ic, key, batch, reply),
            Request::OutputFailed { ic, ctx } => self.fail_output(ic, ctx),
            Request::ClientGone(owner) => {
                let gone: Vec<IcId> = self
                    .ics
                    .iter()
                    .filter(|(_, state)| state.owner == owner)
                    .map(|(ic, _)| *ic)
                    .collect();
                for ic in gone {
                    self.destroy(ic);
                }
            }
        }
    }

    fn focus_in(&mut self, ic: IcId) {
        let Some(state) = self.ics.get_mut(&ic) else {
            return;
        };
        if state.focused {
            return;
        }
        state.focused = true;
        if !convert::content_type(state.capability).is_sensitive() {
            self.start_context(ic);
        }
    }

    fn start_context(&mut self, ic: IcId) {
        let ctx = self.context_ids.next();
        let Some(state) = self.ics.get_mut(&ic) else {
            return;
        };
        state.ctx = Some(ctx);
        debug!(ic, ctx, "text field focused");
        let info = info(state);
        self.events.push_back(FrontendEvent::Activated {
            origin: Origin::Fcitx,
            ctx,
            info,
        });
    }

    fn end_context(&mut self, ic: IcId) {
        let Some(state) = self.ics.get_mut(&ic) else {
            return;
        };
        // A toolkit may reuse its IC for another field. This also clears an
        // unfocused or sensitive IC, which has no core context to deactivate.
        state.surrounding = None;
        state.surrounding_cleared = false;
        if let Some(ctx) = state.ctx.take() {
            debug!(ic, ctx, "text field left");
            state.preedit_shown = false;
            self.events.push_back(FrontendEvent::Deactivated { ctx });
        }
    }

    /// Tells the core that properties of the field changed.
    fn updated(&mut self, ic: IcId, reset: bool) {
        let Some(state) = self.ics.get(&ic) else {
            return;
        };
        if let Some(ctx) = state.ctx {
            self.events.push_back(FrontendEvent::Updated {
                ctx,
                info: info(state),
                reset,
            });
        }
    }

    fn destroy(&mut self, ic: IcId) {
        self.end_context(ic);
        let Some(state) = self.ics.remove(&ic) else {
            return;
        };
        debug!(ic, "input context destroyed");
        if let Some(queue) = &self.queues[state.bus as usize] {
            queue.discard(ic);
        }
        self.settle_pending(ic);
        self.send(state.bus, ic, None, Outgoing::Remove(state.path));
    }

    fn key(&mut self, ic: IcId, key: (u32, u32, u32, bool, u32), batch: bool, reply: KeyReply) {
        // Like fcitx5, a key focuses its context.
        self.focus_in(ic);
        let Some(state) = self.ics.get_mut(&ic) else {
            let _ = reply.send(KeyAnswer::default());
            return;
        };
        let (keyval, keycode, mods, released, time) = key;
        state.last_mods = mods;
        let Some(ctx) = state.ctx else {
            // A password field: the application handles the key itself. The answer
            // goes through the queue, behind anything sent before.
            let bus = state.bus;
            self.send(
                bus,
                ic,
                None,
                Outgoing::Reply {
                    reply,
                    answer: KeyAnswer::default(),
                },
            );
            return;
        };
        if self.pending.len() >= MAX_PENDING_KEYS
            || self
                .pending
                .values()
                .filter(|pending| pending.ic == ic)
                .count()
                >= output::MAX_IC_ITEMS
        {
            self.fail_output(ic, Some(ctx));
            let _ = reply.send(KeyAnswer::default());
            return;
        }
        self.next_key += 1;
        let id = self.next_key;
        self.pending.insert(
            id,
            Pending {
                ic,
                reply,
                key: (keyval, mods, released),
                batch: batch.then(Vec::new),
                produced: false,
            },
        );
        self.events.push_back(FrontendEvent::Key {
            key: frontend::KeyRef::Fcitx(KeyRef { ctx, id }),
            event: crate::keyboard::x11_key_event(keyval, keycode, mods, released, time),
        });
    }

    /// Answers a key. A key that was not consumed but already produced text goes back to
    /// clients with `KeyEventOrderFix` as `ForwardKey`, after that text: they may handle
    /// an unconsumed key before signals that arrived ahead of the reply.
    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        let Some(mut pending) = self.pending.remove(&key.id) else {
            return;
        };
        let Some(state) = self.ics.get(&pending.ic) else {
            // Destroy may precede the core's final decision. Complete the original
            // RPC even though its input context no longer has a signal destination.
            let _ = pending.reply.send(KeyAnswer::default());
            return;
        };
        let (bus, dest, path) = (state.bus, state.owner.clone(), state.path.clone());
        let order_fix = state.capability & convert::capability::KEY_EVENT_ORDER_FIX != 0;
        let mut handled = handled;
        if !handled && order_fix && pending.produced {
            handled = true;
            let (keyval, mods, released) = pending.key;
            let signal = Signal::Forward(keyval, mods, released);
            match &mut pending.batch {
                Some(batch) => {
                    if !push_batch(batch, signal) {
                        self.fail_output(pending.ic, Some(key.ctx));
                        let _ = pending.reply.send(KeyAnswer {
                            handled: true,
                            batch: Vec::new(),
                        });
                        return;
                    }
                }
                None => {
                    if !self.send(
                        bus,
                        pending.ic,
                        Some(key.ctx),
                        Outgoing::Signal { dest, path, signal },
                    ) {
                        let _ = pending.reply.send(KeyAnswer {
                            handled: true,
                            batch: Vec::new(),
                        });
                        return;
                    }
                }
            }
        }
        let answer = KeyAnswer {
            handled,
            batch: pending.batch.unwrap_or_default(),
        };
        self.send(
            bus,
            pending.ic,
            Some(key.ctx),
            Outgoing::Reply {
                reply: pending.reply,
                answer,
            },
        );
    }

    /// Applies an engine's actions to the field `ctx`.
    pub fn apply_actions(
        &mut self,
        ctx: ContextId,
        actions: &[Action],
        reply_key: Option<&KeyRef>,
    ) {
        let modifiers = reply_key
            .and_then(|key| self.pending.get(&key.id))
            .map(|pending| pending.key.1);
        self.apply_actions_with_modifiers(ctx, actions, modifiers);
    }

    pub fn forward_key(&mut self, ctx: ContextId, key: &frontend::ForwardedKey) {
        self.apply_actions_with_modifiers(
            ctx,
            &[Action::ForwardKey {
                keycode: key.keycode,
                pressed: key.pressed,
            }],
            Some(key.modifiers.x11_bits()),
        );
    }

    fn apply_actions_with_modifiers(
        &mut self,
        ctx: ContextId,
        actions: &[Action],
        modifiers: Option<u32>,
    ) {
        // Check the complete transaction before cloning text or changing preedit
        // state. Reserve room for the clear generated by a commit.
        let text_bytes = actions.iter().try_fold(0usize, |bytes, action| {
            bytes.checked_add(match action {
                Action::Commit(text) => text.len(),
                Action::Preedit(preedit) => preedit.text.len(),
                _ => 0,
            })
        });
        if actions.len() >= output::MAX_IC_ITEMS
            || text_bytes.is_none_or(|bytes| bytes > output::MAX_IC_BYTES / 2)
        {
            if let Some((ic, _)) = self.ics.iter().find(|(_, state)| state.ctx == Some(ctx)) {
                self.fail_output(*ic, Some(ctx));
            }
            return;
        }
        let Some((ic, state)) = self.ics.iter_mut().find(|(_, s)| s.ctx == Some(ctx)) else {
            return;
        };
        let ic = *ic;
        let mut signals = Vec::new();
        // A commit replaces the preedit; the protocol needs that said explicitly.
        let mut clear_preedit = false;
        for action in actions {
            match action {
                // Mode confirmations are consumed by Core, never sent to applications.
                Action::InputMode { .. } => {}
                Action::Commit(text) => {
                    signals.push(Signal::Commit(text.clone()));
                    clear_preedit = state.preedit_shown;
                }
                Action::Preedit(preedit) => {
                    let (segments, cursor) = convert::preedit(preedit);
                    signals.push(Signal::Preedit(segments, cursor));
                    state.preedit_shown = !preedit.text.is_empty();
                    clear_preedit = false;
                }
                Action::DeleteSurrounding { before, after } => {
                    let Some(surrounding) = state.surrounding.as_ref().filter(|s| s.valid) else {
                        continue;
                    };
                    let (offset, size) = convert::delete_surrounding(
                        &surrounding.text,
                        surrounding.cursor,
                        *before,
                        *after,
                    );
                    signals.push(Signal::Delete(offset, size));
                }
                Action::ForwardKey { keycode, pressed } => {
                    if self.keymap.is_none() {
                        self.keymap = default_keymap();
                    }
                    if let Some(keymap) = &mut self.keymap {
                        // Reply actions retain their key's state. Host calls use the
                        // last state reported by the application.
                        let mods = modifiers.unwrap_or(state.last_mods);
                        let keyval = keysym(keymap, *keycode, mods);
                        signals.push(Signal::Forward(keyval, mods, !pressed));
                    }
                }
            }
        }
        if clear_preedit {
            signals.push(Signal::Preedit(Vec::new(), 0));
            state.preedit_shown = false;
        }
        self.signal(ic, signals);
    }

    /// Removes any preedit from the field `ctx`.
    pub fn clear_preedit(&mut self, ctx: ContextId) {
        let Some((ic, state)) = self.ics.iter_mut().find(|(_, s)| s.ctx == Some(ctx)) else {
            return;
        };
        if state.preedit_shown {
            state.preedit_shown = false;
            let ic = *ic;
            self.signal(ic, vec![Signal::Preedit(Vec::new(), 0)]);
        }
    }

    /// Sends `signals` to the application of `ic`, or adds them to the reply of the key
    /// its engine is working on, for `ProcessKeyEventBatch`.
    fn signal(&mut self, ic: IcId, signals: Vec<Signal>) {
        if signals.is_empty() {
            return;
        }
        let Some(state) = self.ics.get(&ic) else {
            return;
        };
        let (bus, dest, path, ctx) = (
            state.bus,
            state.owner.clone(),
            state.path.clone(),
            state.ctx,
        );
        // Keys of a field are answered in order, so the oldest one waiting is the one the
        // engine works on (or the next, if this comes from another request).
        if let Some(pending) = self.pending.values_mut().find(|p| p.ic == ic) {
            pending.produced = true;
            if let Some(batch) = &mut pending.batch {
                let admitted = signals.into_iter().all(|signal| push_batch(batch, signal));
                if !admitted {
                    self.fail_output(ic, ctx);
                }
                return;
            }
        }
        for signal in signals {
            if !self.send(
                bus,
                ic,
                ctx,
                Outgoing::Signal {
                    dest: dest.clone(),
                    path: path.clone(),
                    signal,
                },
            ) {
                break;
            }
        }
    }

    fn settle_pending(&mut self, ic: IcId) {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.ic == ic)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(pending) = self.pending.remove(&id) {
                let _ = pending.reply.send(KeyAnswer {
                    handled: pending.produced,
                    batch: Vec::new(),
                });
            }
        }
    }

    fn fail_output(&mut self, ic: IcId, ctx: Option<ContextId>) {
        let Some(state) = self.ics.get(&ic) else {
            return;
        };
        // A delayed write failure must not retire a newly focused field.
        if state.ctx != ctx {
            return;
        }
        if let Some(queue) = &self.queues[state.bus as usize] {
            queue.discard(ic);
        }
        self.settle_pending(ic);
        self.end_context(ic);
        if let Some(state) = self.ics.get_mut(&ic) {
            state.output_retired = true;
        }
        debug!(
            ic,
            "Fcitx output limit reached; composition resumes on the next focus transition"
        );
    }

    fn send(&mut self, bus: Bus, ic: IcId, ctx: Option<ContextId>, item: Outgoing) -> bool {
        let sent = self.queues[bus as usize]
            .as_ref()
            .is_some_and(|queue| queue.send(ic, ctx, item));
        if !sent {
            self.fail_output(ic, ctx);
        }
        sent
    }
}

/// One batch belongs to one IC/key; every non-preedit action is an ordering barrier.
fn push_batch(batch: &mut Vec<Batched>, signal: Signal) -> bool {
    let replace = matches!(
        (&signal, batch.last()),
        (Signal::Preedit(..), Some(Batched(Signal::Preedit(..))))
    );
    let keep = batch.len() - usize::from(replace);
    let bytes = batch
        .iter()
        .take(keep)
        .map(|s| output::signal_bytes(&s.0))
        .sum::<usize>()
        + output::signal_bytes(&signal)
        + batch.capacity().max(keep + 1) * std::mem::size_of::<Batched>();
    if keep + 1 >= output::MAX_IC_ITEMS || bytes > output::MAX_IC_BYTES / 2 {
        return false;
    }
    if replace {
        batch.pop();
    } else {
        // Keep capacity included in the byte budget rather than relying on the
        // vector's geometric growth for many small signals.
        batch.reserve_exact(1);
    }
    batch.push(Batched(signal));
    true
}

/// What the core and engines learn about a field.
fn info(state: &Ic) -> ContextInfo {
    ContextInfo {
        app_id: state.program.clone(),
        content_type: convert::content_type(state.capability),
        surrounding: state
            .surrounding
            .as_ref()
            .map(|s| {
                (
                    s.text.clone(),
                    convert::byte_offset(&s.text, s.cursor),
                    convert::byte_offset(&s.text, s.anchor),
                )
            })
            // Absent update fields preserve engine values. Hosted delivery can use
            // later context info, so keep the clear until text or the field changes.
            .or_else(|| state.surrounding_cleared.then_some((String::new(), 0, 0))),
        cursor_rect: None,
        frontend: Some("fcitx".into()),
        client_preedit: state.capability & convert::capability::PREEDIT != 0,
    }
}

/// The keymap `ForwardKey` actions are interpreted with: the one XKB's defaults
/// (`XKB_DEFAULT_LAYOUT` and friends) describe.
/// The keysym the evdev key `keycode` gives with the X11 modifier mask `mods`.
fn keysym(keymap: &mut xkb::State, keycode: u32, mods: u32) -> u32 {
    // X11's core modifier masks are the bits of xkb's eight real modifiers, which come
    // first in every keymap.
    keymap.update_mask(mods & 0xff, 0, 0, 0, 0, 0);
    keymap.key_get_one_sym(xkb::Keycode::new(keycode + 8)).raw()
}

fn default_keymap() -> Option<xkb::State> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&context, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        .map(|keymap| xkb::State::new(&keymap))
}

/// The text cursor of a Wayland application's field on the screen: its window's
/// position plus the cursor in logical pixels.
fn on_screen(ic: &Ic) -> Option<Rect> {
    let (wx, wy, _, _) = placement::clamp_rect(ic.window?);
    let (x, y, width, height) = placement::clamp_rect(ic.cursor_rect?);
    let scale = cursor_scale(ic.cursor_scale);
    let logical = |v: i32| (f64::from(v) / scale).round() as i64;
    Some(placement::clamp_rect((
        placement::saturate_i32(i64::from(wx).saturating_add(logical(x))),
        placement::saturate_i32(i64::from(wy).saturating_add(logical(y))),
        placement::saturate_i32(logical(width)),
        placement::saturate_i32(logical(height)),
    )))
}

fn cursor_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontend() -> (FcitxFrontend, output::Receiver) {
        let (queue, outgoing) = output::channel();
        let mut frontend = FcitxFrontend::new([Some(queue), None], ContextIds::default());
        frontend.handle(Request::Created {
            ic: 1,
            bus: Bus::Main,
            owner: ":1.1".into(),
            path: OwnedObjectPath::try_from("/ic/1").expect("test path"),
            args: vec![("display".into(), "wayland:".into())],
        });
        (frontend, outgoing)
    }

    fn pending_key(frontend: &mut FcitxFrontend, batch: bool) -> oneshot::Receiver<KeyAnswer> {
        let (reply, answer) = oneshot::channel();
        frontend.handle(Request::Key {
            ic: 1,
            key: (65, 38, 0, false, 0),
            batch,
            reply: reply.into(),
        });
        answer
    }

    #[test]
    fn saturated_output_retires_only_its_context_and_settles_pending_keys() {
        let (mut frontend, mut outgoing) = frontend();
        let mut answer = pending_key(&mut frontend, false);
        let ctx = frontend.ics[&1].ctx.unwrap();
        frontend.handle(Request::Created {
            ic: 2,
            bus: Bus::Main,
            owner: ":1.2".into(),
            path: OwnedObjectPath::try_from("/ic/2").unwrap(),
            args: Vec::new(),
        });
        frontend.handle(Request::FocusIn(2));
        let other = frontend.ics[&2].ctx.unwrap();
        for _ in 0..=output::MAX_IC_ITEMS {
            frontend.apply_actions(ctx, &[Action::Commit("part".into())], None);
        }
        assert!(answer.try_recv().unwrap().handled);
        assert!(frontend.pending.is_empty());
        assert!(frontend.ics[&1].ctx.is_none());
        assert_eq!(frontend.ics[&2].ctx, Some(other));
        for capability in [
            0,
            convert::capability::PASSWORD,
            0,
            convert::capability::PREEDIT,
        ] {
            frontend.handle(Request::Capability { ic: 1, capability });
            assert!(
                frontend.ics[&1].ctx.is_none(),
                "capability churn cannot recover overload retirement"
            );
        }
        assert!(outgoing.try_recv().is_none());
        frontend.apply_actions(other, &[Action::Commit("healthy".into())], None);
        assert!(
            matches!(outgoing.try_recv(), Some(output::Entry { ic: 2, item: Outgoing::Signal { signal: Signal::Commit(text), .. }, .. }) if text == "healthy")
        );
        frontend.handle(Request::FocusIn(1));
        assert!(
            frontend.ics[&1].ctx.is_none(),
            "duplicate focus cannot resume a rejected composition"
        );
        frontend.handle(Request::FocusOut(1));
        frontend.handle(Request::FocusIn(1));
        let fresh = frontend.ics[&1].ctx.unwrap();
        assert_ne!(fresh, ctx);
        frontend.handle(Request::OutputFailed {
            ic: 1,
            ctx: Some(ctx),
        });
        assert_eq!(
            frontend.ics[&1].ctx,
            Some(fresh),
            "late failure belongs to the old field"
        );
    }

    #[test]
    fn batched_preedit_replaces_adjacent_values_but_commits_bound_the_batch() {
        let (mut frontend, _) = frontend();
        let mut answer = pending_key(&mut frontend, true);
        let ctx = frontend.ics[&1].ctx.unwrap();
        for _ in 0..8192 {
            frontend.apply_actions(
                ctx,
                &[Action::Preedit(omime_proto::engine::Preedit {
                    text: "x".repeat(2048),
                    cursor_begin: 2048,
                    cursor_end: 2048,
                })],
                None,
            );
        }
        assert_eq!(
            frontend
                .pending
                .values()
                .next()
                .unwrap()
                .batch
                .as_ref()
                .unwrap()
                .len(),
            1
        );
        for _ in 0..output::MAX_IC_ITEMS {
            frontend.apply_actions(ctx, &[Action::Commit("barrier".into())], None);
        }
        assert!(answer.try_recv().unwrap().handled);
        assert!(frontend.pending.is_empty());
        assert!(frontend.ics[&1].ctx.is_none());
    }

    #[test]
    fn teardown_and_pending_key_limit_release_every_key() {
        let (mut frontend, _) = frontend();
        let mut pending = Vec::new();
        for _ in 0..output::MAX_IC_ITEMS {
            pending.push(pending_key(&mut frontend, false));
        }
        assert_eq!(frontend.pending.len(), output::MAX_IC_ITEMS);
        let mut overflow = pending_key(&mut frontend, false);
        assert!(!overflow.try_recv().unwrap().handled);
        for mut answer in pending {
            assert!(!answer.try_recv().unwrap().handled);
        }
        assert!(frontend.pending.is_empty());
        frontend.handle(Request::FocusOut(1));
        let mut answer = pending_key(&mut frontend, false);
        frontend.handle(Request::Destroy(1));
        assert!(!answer.try_recv().unwrap().handled);
        assert!(frontend.pending.is_empty());
        let (mut frontend, _) = self::frontend();
        let mut answer = pending_key(&mut frontend, true);
        drop(frontend);
        assert!(!answer.try_recv().unwrap().handled);
    }

    #[test]
    fn sensitivity_changes_resume_an_ordinary_context_without_clearing_retirement() {
        let (mut frontend, _) = frontend();
        frontend.handle(Request::FocusIn(1));
        let original = frontend.ics[&1].ctx.unwrap();
        frontend.handle(Request::Capability {
            ic: 1,
            capability: convert::capability::PASSWORD,
        });
        assert!(frontend.ics[&1].ctx.is_none());
        frontend.handle(Request::Capability {
            ic: 1,
            capability: 0,
        });
        let fresh = frontend.ics[&1].ctx.unwrap();
        assert_ne!(original, fresh);
        assert!(!frontend.ics[&1].output_retired);
        frontend.fail_output(1, Some(fresh));
        for capability in [convert::capability::PASSWORD, 0] {
            frontend.handle(Request::Capability { ic: 1, capability });
            assert!(frontend.ics[&1].ctx.is_none());
        }
    }

    #[test]
    fn on_screen_clamps_extreme_rectangles_and_sanitizes_scale() {
        let (mut frontend, _) = frontend();
        for scale in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0, 1.0] {
            frontend.handle(Request::Window {
                ic: 1,
                window: Some((i32::MAX, i32::MIN, 1, 1)),
            });
            frontend.handle(Request::CursorRect {
                ic: 1,
                rect: (i32::MAX, i32::MIN, i32::MAX, i32::MIN),
                scale,
            });
            let ic = &frontend.ics[&1];
            assert_eq!(ic.cursor_rect, Some((1 << 24, -(1 << 24), 1 << 16, 0)));
            assert_eq!(on_screen(ic), Some((1 << 24, -(1 << 24), 1 << 16, 0)));
        }
        frontend.handle(Request::CursorRect {
            ic: 1,
            rect: (i32::MAX, i32::MIN, i32::MAX, i32::MAX),
            scale: f64::MIN_POSITIVE,
        });
        assert_eq!(
            on_screen(&frontend.ics[&1]),
            Some((1 << 24, -(1 << 24), 1 << 16, 1 << 16))
        );
        frontend.handle(Request::Window {
            ic: 1,
            window: Some((100, -100, 1, 1)),
        });
        frontend.handle(Request::CursorRect {
            ic: 1,
            rect: (20, 40, 2, 30),
            scale: 2.0,
        });
        assert_eq!(on_screen(&frontend.ics[&1]), Some((110, -80, 1, 15)));
    }

    #[test]
    fn surrounding_window_keeps_cursor_and_anchor_consistent() {
        let (mut frontend, _) = frontend();
        let text = "a中🙂é".repeat(2_000);
        for cursor in [0, 1, 3_999, 4_000, 7_999, 8_000, u32::MAX] {
            for anchor in [0, cursor.saturating_sub(1), cursor, u32::MAX] {
                frontend.handle(Request::Surrounding {
                    ic: 1,
                    text: Some(text.clone()),
                    cursor,
                    anchor,
                });
                let s = frontend.ics[&1].surrounding.as_ref().expect("surrounding");
                assert!(s.text.len() <= 4_000);
                assert!(
                    s.text.capacity() <= 4_000,
                    "retained the original allocation"
                );
                let start = convert::byte_offset(&text, s.offset) as usize;
                assert_eq!(s.text, text[start..start + s.text.len()]);
                assert_eq!(s.offset + s.cursor, cursor.min(8_000));
                let chars = s.text.chars().count() as u32;
                assert_eq!(s.anchor, anchor.saturating_sub(s.offset).min(chars));
                let (_, byte_cursor, byte_anchor) = info(&frontend.ics[&1])
                    .surrounding
                    .expect("engine surrounding");
                assert_eq!(byte_cursor, convert::byte_offset(&s.text, s.cursor));
                assert_eq!(byte_anchor, convert::byte_offset(&s.text, s.anchor));
            }
        }
    }

    #[test]
    fn surrounding_position_updates_use_the_original_offsets() {
        let (mut frontend, _) = frontend();
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("a中🙂é".repeat(2_000)),
            cursor: 4_000,
            anchor: 4_001,
        });
        let offset = frontend.ics[&1]
            .surrounding
            .as_ref()
            .expect("surrounding")
            .offset;
        assert!(offset > 0);
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: None,
            cursor: 4_002,
            anchor: 4_001,
        });
        let s = frontend.ics[&1].surrounding.as_ref().expect("surrounding");
        assert_eq!(s.offset + s.cursor, 4_002);
        assert_eq!(s.offset + s.anchor, 4_001);
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: None,
            cursor: 0,
            anchor: 0,
        });
        assert_eq!(
            info(&frontend.ics[&1]).surrounding,
            Some((String::new(), 0, 0))
        );
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("a中b".into()),
            cursor: 2,
            anchor: 1,
        });
        assert_eq!(
            info(&frontend.ics[&1]).surrounding,
            Some(("a中b".into(), 4, 1))
        );
    }

    #[test]
    fn delete_surrounding_on_a_windowed_text_counts_characters() {
        let (mut frontend, mut outgoing) = frontend();
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("a中🙂é".repeat(2_000)),
            cursor: 4_002,
            anchor: 4_002,
        });
        frontend.handle(Request::FocusIn(1));
        let ctx = frontend.ics[&1].ctx.expect("focused context");
        let s = frontend.ics[&1].surrounding.as_ref().expect("surrounding");
        assert!(s.text.len() <= 4_000);
        let chars = s.text.chars().count() as u32;
        let cursor = s.cursor;
        frontend.apply_actions(
            ctx,
            &[Action::DeleteSurrounding {
                before: 3,
                after: 4,
            }],
            None,
        );
        assert!(matches!(
            outgoing.try_recv(),
            Some(output::Entry {
                item: Outgoing::Signal {
                    signal: Signal::Delete(-1, 2),
                    ..
                },
                ..
            })
        ));
        frontend.apply_actions(
            ctx,
            &[Action::DeleteSurrounding {
                before: u32::MAX,
                after: u32::MAX,
            }],
            None,
        );
        assert!(
            matches!(outgoing.try_recv(), Some(output::Entry { item: Outgoing::Signal { signal: Signal::Delete(offset, size), .. }, .. }) if offset == -(cursor as i32) && size == chars)
        );
    }

    #[test]
    fn sensitive_surrounding_is_never_cached() {
        for capability in [
            convert::capability::PASSWORD,
            convert::capability::SENSITIVE,
        ] {
            for focused in [false, true] {
                let (mut frontend, _) = frontend();
                frontend.handle(Request::Capability { ic: 1, capability });
                if focused {
                    frontend.handle(Request::FocusIn(1));
                }
                for text in [Some("private text".into()), None] {
                    frontend.handle(Request::Surrounding {
                        ic: 1,
                        text,
                        cursor: 12,
                        anchor: 12,
                    });
                    assert!(frontend.ics[&1].surrounding.is_none());
                    assert!(frontend.take_events().is_empty());
                }
            }
        }
    }

    #[test]
    fn cleared_surrounding_cannot_be_restored_by_position_updates() {
        for focused in [false, true] {
            for clear in [
                Request::FocusOut(1),
                Request::Capability {
                    ic: 1,
                    capability: convert::capability::PASSWORD,
                },
                Request::Capability {
                    ic: 1,
                    capability: convert::capability::SENSITIVE,
                },
                Request::Capability {
                    ic: 1,
                    capability: 0,
                },
            ] {
                let (mut frontend, _) = frontend();
                frontend.handle(Request::Capability {
                    ic: 1,
                    capability: 1 << 6,
                });
                frontend.handle(Request::Surrounding {
                    ic: 1,
                    text: Some("a中b".into()),
                    cursor: 2,
                    anchor: 1,
                });
                if focused {
                    frontend.handle(Request::FocusIn(1));
                }
                frontend.handle(clear);
                assert!(frontend.ics[&1].surrounding.is_none());
                frontend.take_events();
                frontend.handle(Request::Surrounding {
                    ic: 1,
                    text: None,
                    cursor: 2,
                    anchor: 1,
                });
                assert!(frontend.ics[&1].surrounding.is_none());
                assert!(frontend.take_events().is_empty());
            }
        }
    }

    #[test]
    fn reactivation_has_no_surrounding_until_fresh_text_arrives() {
        let (mut frontend, _) = frontend();
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("previous field".into()),
            cursor: 14,
            anchor: 14,
        });
        frontend.handle(Request::FocusIn(1));
        let old = frontend.ics[&1].ctx;
        frontend.handle(Request::FocusOut(1));
        frontend.take_events();
        frontend.handle(Request::FocusIn(1));
        assert_ne!(frontend.ics[&1].ctx, old);
        assert!(
            matches!(frontend.take_events().as_slice(), [FrontendEvent::Activated { info, .. }] if info.surrounding.is_none())
        );
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("a中b".into()),
            cursor: 2,
            anchor: 1,
        });
        assert!(
            matches!(frontend.take_events().as_slice(), [FrontendEvent::Updated { info, .. }] if info.surrounding == Some(("a中b".into(), 4, 1)))
        );
    }

    #[test]
    fn dropping_surrounding_capability_explicitly_clears_engine_text() {
        let (mut frontend, _) = frontend();
        frontend.handle(Request::Capability {
            ic: 1,
            capability: 1 << 6,
        });
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("previous text".into()),
            cursor: 13,
            anchor: 13,
        });
        frontend.handle(Request::FocusIn(1));
        frontend.take_events();
        frontend.handle(Request::Capability {
            ic: 1,
            capability: 0,
        });
        assert!(
            matches!(frontend.take_events().as_slice(), [FrontendEvent::Updated { info, .. }] if info.surrounding == Some((String::new(), 0, 0)))
        );
        assert!(frontend.ics[&1].surrounding.is_none());
        // Hosted delivery may wait for a keyboard grab, then use the latest
        // context info rather than the first queued update's dictionary.
        for request in [
            Request::Reset(1),
            Request::Capability {
                ic: 1,
                capability: convert::capability::PREEDIT,
            },
            Request::Capability {
                ic: 1,
                capability: convert::capability::SURROUNDING_TEXT,
            },
        ] {
            frontend.handle(request);
            assert!(
                matches!(frontend.take_events().as_slice(), [FrontendEvent::Updated { info, .. }] if info.surrounding == Some((String::new(), 0, 0)))
            );
            assert!(frontend.ics[&1].surrounding.is_none());
        }
        frontend.handle(Request::Surrounding {
            ic: 1,
            text: Some("fresh".into()),
            cursor: 5,
            anchor: 5,
        });
        assert!(
            matches!(frontend.take_events().as_slice(), [FrontendEvent::Updated { info, .. }] if info.surrounding == Some(("fresh".into(), 5, 5)))
        );
        frontend.handle(Request::Capability {
            ic: 1,
            capability: 0,
        });
        frontend.handle(Request::FocusOut(1));
        frontend.take_events();
        frontend.handle(Request::FocusIn(1));
        assert!(
            matches!(frontend.take_events().as_slice(), [FrontendEvent::Activated { info, .. }] if info.surrounding.is_none())
        );
    }

    #[test]
    fn delete_surrounding_without_cached_text_sends_no_signal() {
        let (mut frontend, mut outgoing) = frontend();
        frontend.handle(Request::FocusIn(1));
        let ctx = frontend.ics[&1].ctx.expect("focused context");
        frontend.apply_actions(
            ctx,
            &[Action::DeleteSurrounding {
                before: 3,
                after: 4,
            }],
            None,
        );
        assert!(outgoing.try_recv().is_none());
    }

    #[test]
    fn keys_sent_back_get_the_keysym_of_their_modifiers() {
        let mut keymap = default_keymap().expect("xkb's default keymap");
        // evdev 30 is a, 46 is c; X11 masks: Shift 1, Control 4.
        assert_eq!(keysym(&mut keymap, 30, 0), 0x61);
        assert_eq!(keysym(&mut keymap, 30, 1), 0x41);
        assert_eq!(keysym(&mut keymap, 46, 4), 0x63);
        // The state does not stick to the next key.
        assert_eq!(keysym(&mut keymap, 30, 0), 0x61);
    }
}
