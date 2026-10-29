//! The Fcitx5 input method module protocol, for applications that load the Fcitx5 GTK or
//! Qt module (`GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`). It matters most for X11
//! applications under XWayland, which the compositor's text input protocol does not
//! reach.
//!
//! omimed serves the protocol under the names fcitx5 would own on the session bus,
//! `org.fcitx.Fcitx5` and `org.freedesktop.portal.Fcitx` (which Flatpak applications may
//! talk to), and queues for them while another program holds them.

mod convert;
mod service;

use std::collections::{BTreeMap, VecDeque};

use omime_proto::engine::{Action, ContextId, ContextInfo};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use xkbcommon::xkb;
use zbus::OwnedObjectPath;

use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
pub use service::start;

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
        reply: oneshot::Sender<KeyAnswer>,
    },
    /// An application left the bus.
    ClientGone(String),
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
    Reply {
        reply: oneshot::Sender<KeyAnswer>,
        answer: KeyAnswer,
    },
    /// Removes an input context's object from the bus.
    Remove(OwnedObjectPath),
}

#[derive(Debug)]
struct Ic {
    bus: Bus,
    owner: String,
    path: OwnedObjectPath,
    program: Option<String>,
    capability: u64,
    focused: bool,
    /// The core's context while the field has focus and is not sensitive.
    ctx: Option<ContextId>,
    /// Text, cursor and anchor in characters.
    surrounding: Option<(String, u32, u32)>,
    preedit_shown: bool,
}

/// A key the engine has not answered yet.
#[derive(Debug)]
struct Pending {
    ic: IcId,
    reply: oneshot::Sender<KeyAnswer>,
    /// keyval, state, release: what goes back to the application in `ForwardKey`.
    key: (u32, u32, bool),
    /// For `ProcessKeyEventBatch`: what the key produced so far.
    batch: Option<Vec<Batched>>,
    /// Whether anything was sent to the application for this key.
    produced: bool,
}

/// The input contexts of applications using the Fcitx5 module.
pub struct FcitxFrontend {
    queues: [Option<mpsc::UnboundedSender<Outgoing>>; 2],
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

impl FcitxFrontend {
    /// A frontend sending through `queues` (from [`start`]).
    pub fn new(
        queues: [Option<mpsc::UnboundedSender<Outgoing>>; 2],
        context_ids: ContextIds,
    ) -> Self {
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
                        capability: 0,
                        focused: false,
                        ctx: None,
                        surrounding: None,
                        preedit_shown: false,
                    },
                );
            }
            Request::FocusIn(ic) => self.focus_in(ic),
            Request::FocusOut(ic) => {
                if let Some(state) = self.ics.get_mut(&ic) {
                    state.focused = false;
                }
                self.end_context(ic);
            }
            Request::Reset(ic) => self.updated(ic, true),
            Request::Capability { ic, capability } => {
                let Some(state) = self.ics.get_mut(&ic) else {
                    return;
                };
                state.capability = capability;
                // Becoming a password field (or no longer) starts or ends the context.
                let sensitive = convert::content_type(capability).is_sensitive();
                if state.focused && sensitive == state.ctx.is_some() {
                    if sensitive {
                        self.end_context(ic);
                    } else {
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
                let text = match (text, state.surrounding.take()) {
                    (Some(text), _) => text,
                    (None, Some((text, ..))) => text,
                    (None, None) => return,
                };
                state.surrounding = Some((text, cursor, anchor));
                self.updated(ic, false);
            }
            Request::Destroy(ic) => self.destroy(ic),
            Request::Key {
                ic,
                key,
                batch,
                reply,
            } => self.key(ic, key, batch, reply),
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
        self.send(state.bus, Outgoing::Remove(state.path));
    }

    fn key(
        &mut self,
        ic: IcId,
        key: (u32, u32, u32, bool, u32),
        batch: bool,
        reply: oneshot::Sender<KeyAnswer>,
    ) {
        // Like fcitx5, a key focuses its context.
        self.focus_in(ic);
        let Some(state) = self.ics.get(&ic) else {
            let _ = reply.send(KeyAnswer::default());
            return;
        };
        let (keyval, keycode, mods, released, time) = key;
        let Some(ctx) = state.ctx else {
            // A password field: the application handles the key itself. The answer
            // goes through the queue, behind anything sent before.
            let bus = state.bus;
            self.send(
                bus,
                Outgoing::Reply {
                    reply,
                    answer: KeyAnswer::default(),
                },
            );
            return;
        };
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
                Some(batch) => batch.push(Batched(signal)),
                None => self.send(bus, Outgoing::Signal { dest, path, signal }),
            }
        }
        let answer = KeyAnswer {
            handled,
            batch: pending.batch.unwrap_or_default(),
        };
        self.send(
            bus,
            Outgoing::Reply {
                reply: pending.reply,
                answer,
            },
        );
    }

    /// Applies an engine's actions to the field `ctx`.
    pub fn apply_actions(&mut self, ctx: ContextId, actions: &[Action]) {
        let Some((ic, state)) = self.ics.iter_mut().find(|(_, s)| s.ctx == Some(ctx)) else {
            return;
        };
        let ic = *ic;
        let mut signals = Vec::new();
        // A commit replaces the preedit; the protocol needs that said explicitly.
        let mut clear_preedit = false;
        for action in actions {
            match action {
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
                    let surrounding = state
                        .surrounding
                        .as_ref()
                        .map(|(text, cursor, _)| (text.as_str(), *cursor));
                    let (offset, size) = convert::delete_surrounding(surrounding, *before, *after);
                    signals.push(Signal::Delete(offset, size));
                }
                Action::ForwardKey { keycode, pressed } => {
                    if self.keymap.is_none() {
                        self.keymap = default_keymap();
                    }
                    if let Some(keymap) = &self.keymap {
                        let keyval = keymap.key_get_one_sym(xkb::Keycode::new(keycode + 8)).raw();
                        signals.push(Signal::Forward(keyval, 0, !pressed));
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
        let (bus, dest, path) = (state.bus, state.owner.clone(), state.path.clone());
        // Keys of a field are answered in order, so the oldest one waiting is the one the
        // engine works on (or the next, if this comes from another request).
        if let Some(pending) = self.pending.values_mut().find(|p| p.ic == ic) {
            pending.produced = true;
            if let Some(batch) = &mut pending.batch {
                batch.extend(signals.into_iter().map(Batched));
                return;
            }
        }
        for signal in signals {
            self.send(
                bus,
                Outgoing::Signal {
                    dest: dest.clone(),
                    path: path.clone(),
                    signal,
                },
            );
        }
    }

    fn send(&self, bus: Bus, item: Outgoing) {
        if let Some(queue) = &self.queues[bus as usize] {
            let _ = queue.send(item);
        }
    }
}

/// What the core and engines learn about a field.
fn info(state: &Ic) -> ContextInfo {
    ContextInfo {
        app_id: state.program.clone(),
        content_type: convert::content_type(state.capability),
        surrounding: state.surrounding.as_ref().map(|(text, cursor, anchor)| {
            (
                text.clone(),
                convert::byte_offset(text, *cursor),
                convert::byte_offset(text, *anchor),
            )
        }),
        cursor_rect: None,
    }
}

/// The keymap `ForwardKey` actions are interpreted with: the one XKB's defaults
/// (`XKB_DEFAULT_LAYOUT` and friends) describe.
fn default_keymap() -> Option<xkb::State> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&context, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        .map(|keymap| xkb::State::new(&keymap))
}
