//! The core: decides what happens to every key and every focused text field.
//!
//! Each focused text field (context) has a queue of requests for its engine:
//! activation, updates, resets and keys. Requests are sent one at a time, so engines
//! see them in order and their answers apply in order. Hotkeys are evaluated when a key
//! reaches the head of the queue, which keeps switching ordered with typing too.
//!
//! Without an engine (direct input), or when an engine is unavailable, keys pass
//! straight through to the application.

pub mod engines;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::os::fd::OwnedFd;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use omime_proto::config::{Config, HotkeySpec};
use omime_proto::engine::{Action, ContextId, ContextInfo, Dict, KeyEvent, WireAction};
use omime_proto::manifest::InstalledEngine;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use xkbcommon::xkb;

use crate::wayland::{Frontend, FrontendEvent, KeyRef, SeatId};
use engines::{EngineManager, EngineState, HostCall};

/// Events from engines and background tasks, handled on the core's thread.
#[derive(Debug)]
pub enum CoreEvent {
    Registered {
        engine: String,
        conn: zbus::Connection,
    },
    Exited {
        engine: String,
        generation: u64,
        status: Option<ExitStatus>,
    },
    Host {
        engine: String,
        call: HostCall,
    },
    Reply {
        ctx: ContextId,
        token: u64,
        reply: Reply,
    },
}

/// An engine's answer to a request.
#[derive(Debug)]
pub enum Reply {
    Actions(zbus::Result<Vec<WireAction>>),
    Key(zbus::Result<(bool, Vec<WireAction>)>),
}

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyAction {
    /// Direct input ↔ last engine.
    Toggle,
    /// Next enabled engine.
    Next,
}

#[derive(Debug, Clone)]
struct Hotkey {
    modifiers: u32,
    /// Lower-case keysym name, so that Shift does not change the key.
    key: String,
    action: HotkeyAction,
}

fn keysym_key(keysym: u32) -> String {
    xkb::keysym_get_name(xkb::Keysym::new(keysym)).to_ascii_lowercase()
}

/// Modifiers that take part in hotkey matching; lock modifiers are ignored.
const HOTKEY_MODIFIERS: u32 = omime_proto::engine::modifiers::SHIFT
    | omime_proto::engine::modifiers::CONTROL
    | omime_proto::engine::modifiers::ALT
    | omime_proto::engine::modifiers::SUPER;

#[derive(Debug)]
enum Request {
    Activate,
    Update(Dict),
    Reset,
    Key(KeyRef, KeyEvent),
}

#[derive(Debug)]
struct InFlight {
    token: u64,
    engine: String,
    deadline: Instant,
    /// The key waiting for the answer, if the request was a key.
    key: Option<KeyRef>,
}

#[derive(Debug)]
struct Context {
    seat: SeatId,
    info: ContextInfo,
    /// The engine this field talks to; `None` is direct input.
    engine: Option<String>,
    /// Whether the engine has been sent `Activate` for this field.
    activated: bool,
    queue: VecDeque<Request>,
    in_flight: Option<InFlight>,
}

/// Callback for changes of the current engine.
type SwitchListener = Box<dyn Fn(Option<&str>)>;

/// One row of [`Core::engines`].
#[derive(Debug, Clone)]
pub struct EngineSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub state: &'static str,
}

/// The core state.
pub struct Core {
    engines: EngineManager,
    enabled: Vec<String>,
    hotkeys: Vec<Hotkey>,
    key_timeout: Duration,
    /// The engine new and current text fields use; `None` is direct input.
    current: Option<String>,
    /// The engine `toggle` returns to.
    last_engine: Option<String>,
    contexts: BTreeMap<ContextId, Context>,
    events: mpsc::UnboundedSender<CoreEvent>,
    next_token: u64,
    /// Requests that timed out; their late answers are discarded.
    stale_tokens: HashSet<u64>,
    /// Consecutive timeouts per engine.
    timeouts: BTreeMap<String, u32>,
    /// Called whenever `current` changes.
    on_switch: Option<SwitchListener>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("current", &self.current)
            .field("contexts", &self.contexts.len())
            .finish_non_exhaustive()
    }
}

impl Core {
    pub fn new(
        config: &Config,
        installed: Vec<InstalledEngine>,
        events: mpsc::UnboundedSender<CoreEvent>,
        ui_connections: mpsc::UnboundedSender<(String, OwnedFd)>,
        host_display: Option<String>,
    ) -> Self {
        let ids: Vec<String> = installed.iter().map(|e| e.id().to_owned()).collect();
        let enabled = match &config.engines {
            Some(list) => list
                .iter()
                .filter(|id| {
                    let known = ids.contains(id);
                    if !known {
                        warn!(engine = id.as_str(), "configured engine is not installed");
                    }
                    known
                })
                .cloned()
                .collect(),
            None => ids,
        };
        Self {
            engines: EngineManager::new(installed, events.clone(), ui_connections, host_display),
            enabled,
            hotkeys: parse_hotkeys(config),
            key_timeout: Duration::from_millis(config.engine.key_timeout_ms.max(1)),
            current: None,
            last_engine: None,
            contexts: BTreeMap::new(),
            events,
            next_token: 0,
            stale_tokens: HashSet::new(),
            timeouts: BTreeMap::new(),
            on_switch: None,
        }
    }

    /// Registers a callback for changes of the current engine.
    pub fn set_switch_listener(&mut self, listener: impl Fn(Option<&str>) + 'static) {
        self.on_switch = Some(Box::new(listener));
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The status the current engine published.
    pub fn current_status(&self) -> Option<&omime_proto::engine::Status> {
        self.current
            .as_deref()
            .and_then(|id| self.engines.get(id))
            .map(|slot| &slot.status)
    }

    /// All installed engines, enabled ones first in switching order.
    pub fn engines(&self) -> Vec<EngineSummary> {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let summary = |id: &str, enabled: bool| {
            self.engines.get(id).map(|slot| EngineSummary {
                id: id.to_owned(),
                name: slot.installed.manifest.localized_name(&locale).to_owned(),
                enabled,
                state: slot.state.name(),
            })
        };
        let enabled = self.enabled.iter().filter_map(|id| summary(id, true));
        let disabled = self
            .engines
            .ids()
            .filter(|id| !self.enabled.iter().any(|e| e == id))
            .filter_map(|id| summary(id, false));
        enabled.chain(disabled).collect()
    }

    pub fn deadline(&self) -> Option<Instant> {
        let requests = self
            .contexts
            .values()
            .filter_map(|c| c.in_flight.as_ref().map(|f| f.deadline));
        requests.chain(self.engines.deadline()).min()
    }

    pub fn on_timer(&mut self, now: Instant, frontend: &mut Frontend) {
        for failed in self.engines.on_timer(now) {
            self.engine_unavailable(&failed, frontend);
        }
        let expired: Vec<ContextId> = self
            .contexts
            .iter()
            .filter(|(_, c)| c.in_flight.as_ref().is_some_and(|f| f.deadline <= now))
            .map(|(id, _)| *id)
            .collect();
        for ctx in expired {
            let Some(context) = self.contexts.get_mut(&ctx) else {
                continue;
            };
            let Some(flight) = context.in_flight.take() else {
                continue;
            };
            warn!(
                engine = flight.engine,
                ctx, "the engine did not answer in time"
            );
            self.stale_tokens.insert(flight.token);
            if let Some(key) = flight.key {
                frontend.finish_key(&key, false);
            }
            let timeouts = self.timeouts.entry(flight.engine.clone()).or_default();
            *timeouts += 1;
            if *timeouts >= 3 {
                warn!(
                    engine = flight.engine,
                    "the engine keeps timing out; restarting it"
                );
                *timeouts = 0;
                self.engines.restart(&flight.engine);
            }
            self.pump(ctx, frontend);
        }
    }

    pub fn on_frontend_event(&mut self, event: FrontendEvent, frontend: &mut Frontend) {
        match event {
            FrontendEvent::Activated { seat, ctx, info } => {
                let engine = self.current.clone();
                if let Some(id) = &engine
                    && let Err(e) = self.engines.ensure_started(id, false)
                {
                    warn!("{e:#}");
                }
                let mut context = Context {
                    seat,
                    info,
                    engine,
                    activated: false,
                    queue: VecDeque::new(),
                    in_flight: None,
                };
                if context.engine.is_some() {
                    context.queue.push_back(Request::Activate);
                }
                self.contexts.insert(ctx, context);
                self.pump(ctx, frontend);
            }
            FrontendEvent::Updated { ctx, info, reset } => {
                let Some(context) = self.contexts.get_mut(&ctx) else {
                    return;
                };
                let mut info = info;
                // The cursor rectangle comes from the bridge, not from the field.
                info.cursor_rect = context.info.cursor_rect;
                let changes = info.diff(&context.info);
                context.info = info;
                if context.engine.is_some() {
                    if !changes.is_empty() {
                        context.queue.push_back(Request::Update(changes));
                    }
                    if reset {
                        context.queue.push_back(Request::Reset);
                    }
                }
                self.pump(ctx, frontend);
            }
            FrontendEvent::Deactivated { ctx } => {
                if let Some(context) = self.contexts.remove(&ctx) {
                    if let Some(flight) = context.in_flight {
                        self.stale_tokens.insert(flight.token);
                    }
                    if context.activated
                        && let Some(proxy) = context
                            .engine
                            .as_deref()
                            .and_then(|id| self.engines.get(id))
                            .and_then(|slot| slot.proxy.clone())
                    {
                        tokio::task::spawn_local(async move {
                            let _ = proxy.deactivate(ctx).await;
                        });
                    }
                }
            }
            FrontendEvent::Key { key, event } => {
                let Some(context) = self.contexts.get_mut(&key.ctx) else {
                    frontend.finish_key(&key, false);
                    return;
                };
                context.queue.push_back(Request::Key(key, event));
                self.pump(key.ctx, frontend);
            }
            FrontendEvent::CursorRect { engine, rect } => {
                let ids: Vec<ContextId> = self
                    .contexts
                    .iter()
                    .filter(|(_, c)| c.engine.as_deref() == Some(&*engine))
                    .map(|(id, _)| *id)
                    .collect();
                for ctx in ids {
                    let Some(context) = self.contexts.get_mut(&ctx) else {
                        continue;
                    };
                    let mut info = context.info.clone();
                    info.cursor_rect = Some(rect);
                    let changes = info.diff(&context.info);
                    context.info = info;
                    if !changes.is_empty() {
                        context.queue.push_back(Request::Update(changes));
                    }
                    self.pump(ctx, frontend);
                }
            }
        }
    }

    pub async fn on_event(&mut self, event: CoreEvent, frontend: &mut Frontend) {
        match event {
            CoreEvent::Registered { engine, conn } => {
                if let Err(e) = self.engines.registered(&engine, conn).await {
                    warn!(engine, "cannot talk to the engine: {e}");
                    return;
                }
                self.timeouts.remove(&engine);
                let waiting: Vec<ContextId> = self
                    .contexts
                    .iter()
                    .filter(|(_, c)| c.engine.as_deref() == Some(&engine))
                    .map(|(id, _)| *id)
                    .collect();
                for ctx in waiting {
                    if let Some(context) = self.contexts.get_mut(&ctx)
                        && !context.activated
                        && !matches!(context.queue.front(), Some(Request::Activate))
                    {
                        context.queue.push_front(Request::Activate);
                    }
                    self.pump(ctx, frontend);
                }
            }
            CoreEvent::Exited {
                engine,
                generation,
                status,
            } => {
                let needed = self.current.as_deref() == Some(&engine)
                    && self
                        .engines
                        .get(&engine)
                        .is_some_and(|s| !matches!(s.state, EngineState::Stopped));
                self.engines.exited(&engine, generation, status, needed);
                // A new process has to ask for its window again.
                frontend.set_engine_visible(engine.as_str().into(), false);
                self.engine_unavailable(&engine, frontend);
            }
            CoreEvent::Host { engine, call } => self.on_host_call(&engine, call, frontend),
            CoreEvent::Reply { ctx, token, reply } => self.on_reply(ctx, token, reply, frontend),
        }
    }

    fn on_host_call(&mut self, engine: &str, call: HostCall, frontend: &mut Frontend) {
        match call {
            HostCall::Action { ctx, action } => {
                if let Some(context) = self.contexts.get(&ctx)
                    && context.engine.as_deref() == Some(engine)
                {
                    frontend.apply_actions(context.seat, ctx, &[action]);
                }
            }
            HostCall::SetPopupVisible(visible) => {
                frontend.set_engine_visible(engine.into(), visible);
            }
            HostCall::SetStatus(status) => {
                if let Some(slot) = self.engines.get_mut(engine) {
                    slot.status = status;
                }
            }
            HostCall::RequestSwitch(target) => {
                let target = (!target.is_empty()).then_some(target);
                if let Err(e) = self.switch_to(target, frontend) {
                    warn!(engine, "switch requested by engine failed: {e:#}");
                }
            }
        }
    }

    fn on_reply(&mut self, ctx: ContextId, token: u64, reply: Reply, frontend: &mut Frontend) {
        if self.stale_tokens.remove(&token) {
            // The answer came too late; the key already went to the application. Make
            // the engine forget whatever it did with it.
            if let Some(context) = self.contexts.get(&ctx) {
                debug!(ctx, "discarding a late answer");
                let proxy = context
                    .engine
                    .as_deref()
                    .and_then(|id| self.engines.get(id))
                    .and_then(|slot| slot.proxy.clone());
                if let Some(proxy) = proxy {
                    tokio::task::spawn_local(async move {
                        let _ = proxy.reset(ctx).await;
                    });
                }
                frontend.clear_preedit(context.seat, ctx);
            }
            return;
        }
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return;
        };
        let Some(flight) = context.in_flight.take_if(|f| f.token == token) else {
            return;
        };
        self.timeouts.remove(&flight.engine);
        let seat = context.seat;
        let (handled, actions) = match reply {
            Reply::Actions(Ok(actions)) => (false, actions),
            Reply::Key(Ok((handled, actions))) => (handled, actions),
            Reply::Actions(Err(e)) | Reply::Key(Err(e)) => {
                warn!(engine = flight.engine, "engine request failed: {e}");
                (false, Vec::new())
            }
        };
        let actions: Vec<Action> = actions
            .iter()
            .filter_map(|wire| match Action::from_wire(wire) {
                Ok(action) => Some(action),
                Err(e) => {
                    warn!(engine = flight.engine, "ignoring an action: {e}");
                    None
                }
            })
            .collect();
        // Text must reach the application before a key it did not consume.
        frontend.apply_actions(seat, ctx, &actions);
        if let Some(key) = flight.key {
            frontend.finish_key(&key, handled);
        }
        self.pump(ctx, frontend);
    }

    /// An engine went away (crash, failed start): its fields fall back to direct input
    /// until it is back.
    fn engine_unavailable(&mut self, engine: &str, frontend: &mut Frontend) {
        let ids: Vec<ContextId> = self
            .contexts
            .iter()
            .filter(|(_, c)| c.engine.as_deref() == Some(engine))
            .map(|(id, _)| *id)
            .collect();
        for ctx in ids {
            let Some(context) = self.contexts.get_mut(&ctx) else {
                continue;
            };
            if let Some(flight) = context.in_flight.take() {
                self.stale_tokens.insert(flight.token);
                if let Some(key) = flight.key {
                    frontend.finish_key(&key, false);
                }
            }
            // Keys pass through until the engine is back; it is activated again then.
            context.activated = false;
            frontend.clear_preedit(context.seat, ctx);
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            self.pump(ctx, frontend);
        }
    }

    /// Sends queued requests of `ctx` while nothing is in flight.
    fn pump(&mut self, ctx: ContextId, frontend: &mut Frontend) {
        loop {
            let Some(context) = self.contexts.get_mut(&ctx) else {
                return;
            };
            if context.in_flight.is_some() {
                return;
            }
            let Some(request) = context.queue.front() else {
                return;
            };

            // Hotkeys are handled in order with the keys around them.
            if let Request::Key(key, event) = request
                && let Some(action) = match_hotkey(&self.hotkeys, event)
            {
                let key = *key;
                context.queue.pop_front();
                frontend.finish_key(&key, true);
                let result = match action {
                    HotkeyAction::Toggle => self.toggle(frontend),
                    HotkeyAction::Next => self.next(frontend),
                };
                if let Err(e) = result {
                    warn!("{e:#}");
                }
                continue;
            }

            let state = context
                .engine
                .as_deref()
                .and_then(|id| self.engines.get(id))
                .map(|slot| slot.state);
            match state {
                Some(EngineState::Ready) => {}
                // Hold requests while the engine starts.
                Some(EngineState::Starting { .. }) => return,
                // No engine, or it is unavailable: keys pass straight through and
                // other requests are dropped (the engine is activated afresh later).
                _ => {
                    if let Some(Request::Key(key, _)) = context.queue.pop_front() {
                        frontend.finish_key(&key, false);
                    }
                    continue;
                }
            }
            let Some(request) = context.queue.pop_front() else {
                return;
            };
            self.send(ctx, request);
        }
    }

    fn send(&mut self, ctx: ContextId, request: Request) {
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return;
        };
        let Some(engine) = context.engine.clone() else {
            return;
        };
        let Some(proxy) = self.engines.get(&engine).and_then(|s| s.proxy.clone()) else {
            return;
        };
        self.next_token += 1;
        let token = self.next_token;
        let events = self.events.clone();
        let mut key_ref = None;
        match request {
            Request::Activate => {
                context.activated = true;
                let info = context.info.to_dict();
                tokio::task::spawn_local(async move {
                    let reply = Reply::Actions(proxy.activate(ctx, &info).await);
                    let _ = events.send(CoreEvent::Reply { ctx, token, reply });
                });
            }
            Request::Update(changes) => {
                tokio::task::spawn_local(async move {
                    let reply = Reply::Actions(proxy.update_context(ctx, &changes).await);
                    let _ = events.send(CoreEvent::Reply { ctx, token, reply });
                });
            }
            Request::Reset => {
                tokio::task::spawn_local(async move {
                    let reply = Reply::Actions(proxy.reset(ctx).await);
                    let _ = events.send(CoreEvent::Reply { ctx, token, reply });
                });
            }
            Request::Key(key, event) => {
                key_ref = Some(key);
                tokio::task::spawn_local(async move {
                    let reply = Reply::Key(proxy.process_key(ctx, &event, &Dict::new()).await);
                    let _ = events.send(CoreEvent::Reply { ctx, token, reply });
                });
            }
        }
        context.in_flight = Some(InFlight {
            token,
            engine,
            deadline: Instant::now() + self.key_timeout,
            key: key_ref,
        });
    }

    /// Makes `target` (or direct input) the engine of all fields.
    pub fn switch_to(
        &mut self,
        target: Option<String>,
        frontend: &mut Frontend,
    ) -> anyhow::Result<()> {
        if let Some(id) = &target {
            anyhow::ensure!(
                self.engines.get(id).is_some(),
                "no engine `{id}` is installed"
            );
            // An explicit switch also retries an engine that gave up after crashing.
            self.engines.ensure_started(id, true)?;
        }
        if target == self.current {
            return Ok(());
        }
        info!(engine = target.as_deref().unwrap_or("direct"), "switching");
        if let Some(previous) = self.current.take() {
            self.last_engine = Some(previous);
        }
        self.current = target.clone();
        frontend.set_active_engine(target.as_deref().map(Into::into));

        let ids: Vec<ContextId> = self.contexts.keys().copied().collect();
        for ctx in ids {
            let Some(context) = self.contexts.get_mut(&ctx) else {
                continue;
            };
            if context.engine == target {
                continue;
            }
            // Leave the old engine: its preedit goes, and so does anything it still
            // had queued that is not a key.
            if context.activated
                && let Some(proxy) = context
                    .engine
                    .as_deref()
                    .and_then(|id| self.engines.get(id))
                    .and_then(|slot| slot.proxy.clone())
            {
                tokio::task::spawn_local(async move {
                    let _ = proxy.deactivate(ctx).await;
                });
            }
            frontend.clear_preedit(context.seat, ctx);
            if let Some(flight) = context.in_flight.take() {
                self.stale_tokens.insert(flight.token);
                if let Some(key) = flight.key {
                    frontend.finish_key(&key, false);
                }
            }
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            context.activated = false;
            context.engine = target.clone();
            if target.is_some() {
                context.queue.push_front(Request::Activate);
            }
        }
        if let Some(listener) = &self.on_switch {
            listener(self.current.as_deref());
        }
        Ok(())
    }

    /// Switches to the next enabled engine (wrapping around to the first).
    pub fn next(&mut self, frontend: &mut Frontend) -> anyhow::Result<()> {
        anyhow::ensure!(!self.enabled.is_empty(), "no engines are enabled");
        let position = self
            .current
            .as_ref()
            .and_then(|c| self.enabled.iter().position(|e| e == c));
        let next = match position {
            Some(i) => self.enabled[(i + 1) % self.enabled.len()].clone(),
            None => self.enabled[0].clone(),
        };
        self.switch_to(Some(next), frontend)
    }

    /// Switches between direct input and the last engine.
    pub fn toggle(&mut self, frontend: &mut Frontend) -> anyhow::Result<()> {
        if self.current.is_some() {
            return self.switch_to(None, frontend);
        }
        let target = self
            .last_engine
            .clone()
            .filter(|e| self.enabled.contains(e))
            .or_else(|| self.enabled.first().cloned());
        match target {
            Some(engine) => self.switch_to(Some(engine), frontend),
            None => anyhow::bail!("no engines are enabled"),
        }
    }

    /// Forgets all text fields, e.g. after the compositor connection was lost.
    pub fn forget_contexts(&mut self) {
        for (_, context) in std::mem::take(&mut self.contexts) {
            if let Some(flight) = context.in_flight {
                self.stale_tokens.insert(flight.token);
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.engines.shutdown();
    }
}

fn match_hotkey(hotkeys: &[Hotkey], event: &KeyEvent) -> Option<HotkeyAction> {
    if event.released || event.repeat {
        return None;
    }
    let modifiers = event.modifiers & HOTKEY_MODIFIERS;
    if modifiers == 0 && hotkeys.iter().all(|h| h.modifiers != 0) {
        return None;
    }
    let key = keysym_key(event.keysym);
    hotkeys
        .iter()
        .find(|h| h.modifiers == modifiers && h.key == key)
        .map(|h| h.action)
}

fn parse_hotkeys(config: &Config) -> Vec<Hotkey> {
    let mut hotkeys = Vec::new();
    for (text, action) in [
        (&config.hotkeys.toggle, HotkeyAction::Toggle),
        (&config.hotkeys.next, HotkeyAction::Next),
    ] {
        match HotkeySpec::parse(text) {
            Ok(Some(spec)) => {
                let keysym = xkb::keysym_from_name(&spec.keysym_name, xkb::KEYSYM_CASE_INSENSITIVE);
                if keysym.raw() == 0 {
                    warn!("unknown key `{}` in hotkey `{text}`", spec.keysym_name);
                    continue;
                }
                hotkeys.push(Hotkey {
                    modifiers: spec.modifiers,
                    key: keysym_key(keysym.raw()),
                    action,
                });
            }
            Ok(None) => {}
            Err(e) => warn!("ignoring hotkey: {e}"),
        }
    }
    hotkeys
}
