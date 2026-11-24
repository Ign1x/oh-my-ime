//! Native IBus engine frontend. It connects to the existing IBus daemon; it never
//! owns IBus's name, starts an IBus daemon, or claims compositor input protocols.
mod service;
mod wire;
use crate::frontend;
use crate::frontend::{ContextIds, FrontendEvent, Origin};
use omime_proto::engine::{Action, CandidatePage, ContextId, ContextInfo, KeyEvent};
use omime_proto::{ContentHints, ContentPurpose, ContentType};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{mpsc, oneshot};

pub const COMPONENT: &str = "org.freedesktop.IBus.OhMyIme";
pub const ENGINE: &str = "oh-my-ime";
pub type IcId = u64;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRef {
    pub ctx: ContextId,
    id: u64,
}
#[derive(Debug)]
pub enum Request {
    Connected(mpsc::Sender<Outgoing>),
    Disconnected,
    Created {
        ic: IcId,
        path: zbus::OwnedObjectPath,
        live: Arc<AtomicU64>,
    },
    Focus(IcId, bool, Option<String>),
    Enable(IcId, bool),
    Reset(IcId),
    Destroy(IcId),
    Content(IcId, u32, u32),
    Capabilities(IcId, u32),
    Surrounding(IcId, String, u32, u32),
    Cursor(IcId, (i32, i32, i32, i32)),
    Key {
        ic: IcId,
        keyval: u32,
        keycode: u32,
        state: u32,
        reply: oneshot::Sender<bool>,
        id: u64,
        produced: Arc<AtomicBool>,
    },
    Expired(IcId),
    Candidate(IcId, u32, u32, u32),
    Page(IcId, bool),
}
#[derive(Debug)]
pub enum Signal {
    Commit(String),
    Preedit(String, u32),
    Delete(i32, u32),
    Forward(u32, u32, u32),
    Candidates(CandidatePage),
}
#[derive(Debug)]
pub enum Outgoing {
    Signal {
        path: zbus::OwnedObjectPath,
        live: Arc<AtomicU64>,
        ctx: ContextId,
        signal: Signal,
        produced: Option<Arc<AtomicBool>>,
    },
    Reply(oneshot::Sender<bool>, bool, Arc<AtomicBool>),
}
struct Ic {
    path: zbus::OwnedObjectPath,
    live: Arc<AtomicU64>,
    focused: bool,
    enabled: bool,
    ctx: Option<ContextId>,
    info: ContextInfo,
    caps: u32,
    last_state: u32,
}
struct Pending {
    ic: IcId,
    reply: oneshot::Sender<bool>,
    produced: Arc<AtomicBool>,
    keyval: u32,
    keycode: u32,
    state: u32,
}
pub struct IbusFrontend {
    ids: ContextIds,
    contexts: BTreeMap<IcId, Ic>,
    keys: BTreeMap<u64, Pending>,
    events: VecDeque<FrontendEvent>,
    output: Option<mpsc::Sender<Outgoing>>,
    keymap: Option<xkbcommon::xkb::State>,
}
impl IbusFrontend {
    pub fn start(ids: ContextIds) -> (Self, mpsc::Receiver<Request>) {
        let (tx, rx) = mpsc::channel(256);
        tokio::task::spawn_local(service::run(tx));
        (
            Self {
                ids,
                contexts: BTreeMap::new(),
                keys: BTreeMap::new(),
                events: VecDeque::new(),
                output: None,
                keymap: default_keymap(),
            },
            rx,
        )
    }
    fn ic_for_context(&self, ctx: ContextId) -> Option<&Ic> {
        self.contexts
            .values()
            .find(|context| context.ctx == Some(ctx) && context.live.load(Ordering::Acquire) == ctx)
    }
    pub fn is_connected(&self) -> bool {
        self.output
            .as_ref()
            .is_some_and(|output| !output.is_closed())
    }
    pub fn is_selected(&self) -> bool {
        self.is_connected() && self.contexts.values().any(|context| context.enabled)
    }

    pub fn take_events(&mut self) -> impl Iterator<Item = FrontendEvent> + '_ {
        self.events.drain(..)
    }
    pub fn handle(&mut self, request: Request) {
        match request {
            Request::Connected(output) => self.output = Some(output),
            Request::Disconnected => {
                self.output = None;
                let ids: Vec<_> = self.contexts.keys().copied().collect();
                for ic in ids {
                    self.deactivate(ic);
                }
                self.contexts.clear();
            }
            Request::Created { ic, path, live } => {
                self.contexts.insert(
                    ic,
                    Ic {
                        path,
                        live,
                        focused: false,
                        enabled: false,
                        ctx: None,
                        info: ContextInfo {
                            frontend: Some("ibus".into()),
                            ..Default::default()
                        },
                        caps: 0,
                        last_state: 0,
                    },
                );
            }
            Request::Focus(ic, focus, app) => {
                if focus {
                    let ids: Vec<_> = self
                        .contexts
                        .keys()
                        .copied()
                        .filter(|id| *id != ic)
                        .collect();
                    for id in ids {
                        self.deactivate(id);
                        if let Some(c) = self.contexts.get_mut(&id) {
                            c.focused = false;
                        }
                    }
                }
                self.deactivate(ic);
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.focused = focus;
                    c.info.app_id = app;
                    c.info.surrounding = None;
                }
                if focus {
                    self.activate(ic);
                }
            }
            Request::Enable(ic, enabled) => {
                if !enabled {
                    self.deactivate(ic);
                }
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.enabled = enabled;
                }
                if enabled {
                    self.activate(ic);
                }
            }
            Request::Destroy(ic) => {
                self.deactivate(ic);
                self.contexts.remove(&ic);
            }
            Request::Expired(ic) => {
                self.deactivate(ic);
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.focused = false;
                }
            }
            Request::Content(ic, purpose, hints) => {
                let content = content_type(purpose, hints);
                if content.is_sensitive() {
                    self.deactivate(ic);
                }
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.info.content_type = content;
                    if content.is_sensitive() {
                        c.info.surrounding = Some((String::new(), 0, 0));
                    }
                }
                self.activate(ic);
                self.update(ic, false);
            }
            Request::Capabilities(ic, caps) => {
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.caps = caps;
                    c.info.client_preedit = caps & 1 != 0;
                    if caps & (1 << 5) == 0 {
                        c.info.surrounding = Some((String::new(), 0, 0));
                    }
                }
                self.update(ic, false);
            }
            Request::Surrounding(ic, text, cursor, anchor) => {
                if let Some(c) = self.contexts.get_mut(&ic)
                    && c.ctx.is_some()
                    && c.caps & (1 << 5) != 0
                    && !c.info.content_type.is_sensitive()
                {
                    let cursor = wire::byte_offset(&text, cursor);
                    let anchor = wire::byte_offset(&text, anchor);
                    // Keep a bounded UTF-8 window around the cursor.
                    let mut start = cursor
                        .saturating_sub(2000)
                        .min(text.len().saturating_sub(4000));
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    let mut end = (start + 4000).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    c.info.surrounding = Some((
                        text[start..end].into(),
                        cursor.saturating_sub(start).min(end - start) as u32,
                        anchor.saturating_sub(start).min(end - start) as u32,
                    ));
                }
                self.update(ic, false);
            }
            Request::Cursor(ic, rect) => {
                if let Some(c) = self.contexts.get_mut(&ic) {
                    c.info.cursor_rect = Some(rect);
                }
                self.update(ic, false);
            }
            Request::Reset(ic) => self.update(ic, true),
            Request::Key {
                ic,
                keyval,
                keycode,
                state,
                reply,
                id,
                produced,
            } => {
                let Some(c) = self.contexts.get_mut(&ic).filter(|c| {
                    c.ctx
                        .is_some_and(|ctx| c.live.load(Ordering::Acquire) == ctx)
                }) else {
                    let _ = reply.send(false);
                    return;
                };
                if self.keys.len() >= 256 {
                    let _ = reply.send(false);
                    self.deactivate(ic);
                    return;
                }
                let ctx = c.ctx.expect("live IBus contexts have a context id");
                c.last_state = state;
                self.keys.insert(
                    id,
                    Pending {
                        ic,
                        reply,
                        produced,
                        keyval,
                        keycode,
                        state,
                    },
                );
                let text = if state & ((1 << 2) | (1 << 3) | (1 << 6)) == 0 {
                    xkbcommon::xkb::keysym_to_utf8(xkbcommon::xkb::Keysym::new(keyval))
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect()
                } else {
                    String::new()
                };
                self.events.push_back(FrontendEvent::Key {
                    key: frontend::KeyRef::Ibus(KeyRef { ctx, id }),
                    event: KeyEvent {
                        keysym: keyval,
                        keycode,
                        modifiers: state & 0xff,
                        time: 0,
                        text,
                        released: state & (1 << 30) != 0,
                        repeat: false,
                    },
                });
            }
            Request::Candidate(ic, index, button, modifiers) => {
                if let Some(ctx) = self.contexts.get(&ic).and_then(|c| c.ctx) {
                    self.events.push_back(FrontendEvent::CandidateClicked {
                        ctx,
                        index,
                        button,
                        modifiers,
                    });
                }
            }
            Request::Page(ic, previous) => {
                if let Some(ctx) = self.contexts.get(&ic).and_then(|c| c.ctx) {
                    self.events
                        .push_back(FrontendEvent::CandidatePage { ctx, previous });
                }
            }
        }
    }
    fn activate(&mut self, ic: IcId) {
        if let Some(c) = self.contexts.get_mut(&ic)
            && c.enabled
            && c.focused
            && c.ctx.is_none()
            && !c.info.content_type.is_sensitive()
        {
            let ctx = self.ids.next();
            c.ctx = Some(ctx);
            c.live.store(ctx, Ordering::Release);
            self.events.push_back(FrontendEvent::Activated {
                origin: Origin::Ibus,
                ctx,
                info: c.info.clone(),
            });
        }
    }
    fn update(&mut self, ic: IcId, reset: bool) {
        if let Some(c) = self.contexts.get(&ic)
            && let Some(ctx) = c.ctx
        {
            self.events.push_back(FrontendEvent::Updated {
                ctx,
                info: c.info.clone(),
                reset,
            });
        }
    }
    fn deactivate(&mut self, ic: IcId) {
        if let Some(c) = self.contexts.get_mut(&ic)
            && let Some(ctx) = c.ctx.take()
        {
            c.live.store(0, Ordering::Release);
            c.info.surrounding = None;
            self.events.push_back(FrontendEvent::Deactivated { ctx });
        }
        let keys: Vec<_> = self
            .keys
            .iter()
            .filter(|(_, k)| k.ic == ic)
            .map(|(id, _)| *id)
            .collect();
        for id in keys {
            if let Some(k) = self.keys.remove(&id) {
                let _ = k.reply.send(k.produced.load(Ordering::Acquire));
            }
        }
    }
    fn signal(&mut self, ctx: ContextId, signal: Signal) {
        self.signal_for_key(ctx, signal, None);
    }
    fn signal_for_key(&mut self, ctx: ContextId, signal: Signal, key: Option<&KeyRef>) {
        let Some((ic, c)) = self
            .contexts
            .iter()
            .find(|(_, c)| c.ctx == Some(ctx) && c.live.load(Ordering::Acquire) == ctx)
        else {
            return;
        };
        let ic = *ic;
        let outgoing = Outgoing::Signal {
            path: c.path.clone(),
            live: c.live.clone(),
            ctx,
            produced: key
                .and_then(|key| self.keys.get(&key.id))
                .map(|key| key.produced.clone()),
            signal,
        };
        if self
            .output
            .as_ref()
            .is_none_or(|out| out.try_send(outgoing).is_err())
        {
            self.deactivate(ic);
        }
    }
    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        if let Some(k) = self.keys.remove(&key.id) {
            // A completed request may commit composition and leave the current key
            // unhandled. IBus must still pass that key to the application. Only
            // cancellation/transport failure uses the produced-output fallback.
            let item = Outgoing::Reply(k.reply, handled, k.produced);
            if let Some(out) = &self.output {
                if let Err(e) = out.try_send(item)
                    && let Outgoing::Reply(reply, handled, produced) = e.into_inner()
                {
                    let _ = reply.send(handled || produced.load(Ordering::Acquire));
                }
            } else if let Outgoing::Reply(reply, handled, produced) = item {
                let _ = reply.send(handled || produced.load(Ordering::Acquire));
            }
        }
    }
    pub fn apply_actions(&mut self, ctx: ContextId, actions: &[Action], key: Option<&KeyRef>) {
        for action in actions.iter().take(64) {
            let signal = match action {
                Action::Commit(text) if text.len() <= 32768 => Some(Signal::Commit(text.clone())),
                Action::Preedit(p) if p.text.len() <= 32768 => Some(Signal::Preedit(
                    p.text.clone(),
                    wire::char_offset(&p.text, p.cursor_begin.max(0) as usize),
                )),
                Action::DeleteSurrounding { before, after } => self
                    .ic_for_context(ctx)
                    .and_then(|c| c.info.surrounding.as_ref())
                    .and_then(|(text, cursor, _)| {
                        let cursor = *cursor as usize;
                        let start = cursor.checked_sub(*before as usize)?;
                        let end = cursor.checked_add(*after as usize)?;
                        if end > text.len()
                            || !text.is_char_boundary(start)
                            || !text.is_char_boundary(end)
                        {
                            return None;
                        }
                        let before = text[start..cursor].chars().count() as i32;
                        Some(Signal::Delete(
                            -before,
                            text[start..end].chars().count() as u32,
                        ))
                    }),
                Action::ForwardKey { keycode, pressed } => {
                    let pending = key.and_then(|key| self.keys.get(&key.id));
                    let state = pending
                        .map(|p| p.state)
                        .or_else(|| {
                            self.contexts
                                .values()
                                .find(|c| c.ctx == Some(ctx))
                                .map(|c| c.last_state)
                        })
                        .unwrap_or(0)
                        & !(1 << 30);
                    let keyval = pending
                        .filter(|p| p.keycode == *keycode)
                        .map(|p| p.keyval)
                        .unwrap_or_else(|| {
                            self.keymap.as_mut().map_or(0, |map| {
                                map.update_mask(state & 0xff, 0, 0, 0, 0, 0);
                                map.key_get_one_sym(xkbcommon::xkb::Keycode::new(
                                    keycode.saturating_add(8),
                                ))
                                .raw()
                            })
                        });
                    Some(Signal::Forward(
                        keyval,
                        *keycode,
                        state | if *pressed { 0 } else { 1 << 30 },
                    ))
                }
                _ => None,
            };
            if let Some(signal) = signal {
                self.signal_for_key(ctx, signal, key);
            }
        }
    }
    pub fn clear_preedit(&mut self, ctx: ContextId) {
        self.signal(ctx, Signal::Preedit(String::new(), 0));
        self.set_candidates(ctx, &CandidatePage::default());
    }
    pub fn forward_key(&mut self, ctx: ContextId, key: &crate::frontend::ForwardedKey) {
        self.signal(
            ctx,
            Signal::Forward(0, key.keycode, if key.pressed { 0 } else { 1 << 30 }),
        );
    }
    pub fn set_candidates(&mut self, ctx: ContextId, page: &CandidatePage) {
        self.signal(ctx, Signal::Candidates(page.clone()));
    }
}
fn content_type(purpose: u32, hints: u32) -> ContentType {
    ContentType {
        purpose: match purpose {
            0 => ContentPurpose::Normal,
            1 => ContentPurpose::Alpha,
            2 => ContentPurpose::Digits,
            3 => ContentPurpose::Number,
            4 => ContentPurpose::Phone,
            5 => ContentPurpose::Url,
            6 => ContentPurpose::Email,
            7 => ContentPurpose::Name,
            8 => ContentPurpose::Password,
            9 => ContentPurpose::Pin,
            10 => ContentPurpose::Terminal,
            11 => ContentPurpose::Date,
            12 => ContentPurpose::Time,
            13 => ContentPurpose::DateTime,
            other => ContentPurpose::Unknown(other),
        },
        hints: ContentHints(if hints & ((1 << 11) | (1 << 12)) != 0 {
            ContentHints::SENSITIVE_DATA.0
        } else {
            0
        }),
    }
}

fn default_keymap() -> Option<xkbcommon::xkb::State> {
    use xkbcommon::xkb;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&context, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        .map(|keymap| xkb::State::new(&keymap))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frontend() -> (IbusFrontend, mpsc::Receiver<Outgoing>) {
        let (output, receive) = mpsc::channel(32);
        let mut frontend = IbusFrontend {
            ids: ContextIds::default(),
            contexts: BTreeMap::new(),
            keys: BTreeMap::new(),
            events: VecDeque::new(),
            output: Some(output),
            keymap: default_keymap(),
        };
        frontend.handle(Request::Created {
            ic: 1,
            path: zbus::OwnedObjectPath::try_from("/org/freedesktop/IBus/Engine/test").unwrap(),
            live: Arc::new(AtomicU64::new(0)),
        });
        frontend.handle(Request::Enable(1, true));
        frontend.handle(Request::Focus(1, true, Some("editor".into())));
        (frontend, receive)
    }
    #[test]
    fn password_ends_context_and_refocus_does_not_restore_surrounding() {
        let (mut frontend, mut output) = frontend();
        let ctx = frontend.contexts[&1].ctx.unwrap();
        frontend.handle(Request::Capabilities(1, 1 | (1 << 5)));
        frontend.handle(Request::Surrounding(1, "a你🙂z".into(), 3, 3));
        assert_eq!(
            frontend.contexts[&1].info.surrounding.as_ref().unwrap().1,
            8
        );
        frontend.handle(Request::Content(1, 8, 0));
        assert!(frontend.contexts[&1].ctx.is_none());
        let (reply, mut receive) = oneshot::channel();
        frontend.handle(Request::Key {
            ic: 1,
            keyval: 97,
            keycode: 30,
            state: 0,
            reply,
            id: 1,
            produced: Arc::new(AtomicBool::new(false)),
        });
        assert!(!receive.try_recv().unwrap());
        frontend.apply_actions(ctx, &[Action::Commit("stale".into())], None);
        assert!(output.try_recv().is_err());
        frontend.handle(Request::Content(1, 0, 0));
        assert_ne!(frontend.contexts[&1].ctx.unwrap(), ctx);
        assert!(
            frontend.contexts[&1]
                .info
                .surrounding
                .as_ref()
                .unwrap()
                .0
                .is_empty()
        );
    }
    #[test]
    fn byte_deletions_become_character_counts_and_capability_clear_reaches_core() {
        let (mut frontend, mut output) = frontend();
        let ctx = frontend.contexts[&1].ctx.unwrap();
        frontend.handle(Request::Capabilities(1, 1 | (1 << 5)));
        frontend.handle(Request::Surrounding(1, "a你🙂z".into(), 3, 3));
        frontend.apply_actions(
            ctx,
            &[Action::DeleteSurrounding {
                before: 7,
                after: 1,
            }],
            None,
        );
        assert!(matches!(
            output.try_recv().unwrap(),
            Outgoing::Signal {
                signal: Signal::Delete(-2, 3),
                ..
            }
        ));
        frontend.handle(Request::Capabilities(1, 1));
        assert_eq!(
            frontend.contexts[&1].info.surrounding,
            Some((String::new(), 0, 0))
        );
        frontend.apply_actions(
            ctx,
            &[Action::DeleteSurrounding {
                before: 1,
                after: 0,
            }],
            None,
        );
        assert!(output.try_recv().is_err());
    }
    #[test]
    fn output_is_associated_only_with_its_key_and_forward_keeps_snapshot_keysym() {
        let (mut frontend, mut output) = frontend();
        let ctx = frontend.contexts[&1].ctx.unwrap();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        for (id, keyval, state, produced) in [(1, 65, 1, first.clone()), (2, 98, 0, second.clone())]
        {
            let (reply, _) = oneshot::channel();
            frontend.handle(Request::Key {
                ic: 1,
                keyval,
                keycode: 30,
                state,
                reply,
                id,
                produced,
            });
        }
        let key = KeyRef { ctx, id: 1 };
        frontend.apply_actions(
            ctx,
            &[Action::ForwardKey {
                keycode: 30,
                pressed: true,
            }],
            Some(&key),
        );
        match output.try_recv().unwrap() {
            Outgoing::Signal {
                signal: Signal::Forward(65, 30, 1),
                produced: Some(produced),
                ..
            } => produced.store(true, Ordering::Release),
            _ => panic!("forward lost key snapshot"),
        }
        assert!(first.load(Ordering::Acquire));
        assert!(!second.load(Ordering::Acquire));
    }
    #[test]
    fn focus_loss_invalidates_already_queued_output() {
        let (mut frontend, mut output) = frontend();
        let ctx = frontend.contexts[&1].ctx.unwrap();
        frontend.apply_actions(ctx, &[Action::Commit("hello".into())], None);
        frontend.handle(Request::Focus(1, false, None));
        match output.try_recv().unwrap() {
            Outgoing::Signal { live, ctx, .. } => assert_ne!(live.load(Ordering::Acquire), ctx),
            _ => panic!("expected text"),
        }
    }
}
