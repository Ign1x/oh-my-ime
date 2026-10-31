//! The core: decides what happens to every key and every focused text field.
//!
//! Each focused text field (context) has a queue of requests for its engine:
//! activation, updates, resets and keys. Requests are sent one at a time, so engines
//! see them in order and their answers apply in order. Hotkeys are evaluated when a key
//! reaches the head of the queue, which keeps switching ordered with typing too.
//!
//! Without an engine (direct input), or when an engine is unavailable, keys pass
//! straight through to the application.
//!
//! Hosted input methods (Wayland input methods running as engines) take requests
//! through the frontends right away and answer when they like, with
//! [`HostedEvent::Actions`]; their contexts never have a request in flight.

mod bus;
pub mod engines;
pub mod frameworks;
pub mod sni_watcher;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use omime_proto::config::{Config, HotkeySpec};
use omime_proto::engine::{Action, ContextId, ContextInfo, Dict, KeyEvent, Status, WireAction};
use omime_proto::manifest::InstalledEngine;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use xkbcommon::xkb;

use crate::frontend::{FrontendEvent, Frontends, HostedEvent, KeyDelivery, KeyRef, Origin};
use crate::state::State;
use crate::wayland::EngineId;
use engines::{EngineManager, EngineState, HostCall, UiConnection};

/// How long keys wait for a hosted input method to start, or to grab the keyboard
/// once it was activated, before they pass through.
const HOSTED_WAIT: Duration = Duration::from_secs(1);

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
    /// The tray item of a hosted engine (launch `generation`) changed, or went away.
    HostedTray {
        engine: String,
        generation: u64,
        tray: Option<sni_watcher::HostedTray>,
    },
    /// The adapter of a hosted framework (launch `generation`) learnt something.
    Framework {
        engine: String,
        generation: u64,
        update: frameworks::FrameworkUpdate,
    },
}

/// What users choose: an engine of omimed's, or an input method inside a hosted
/// framework (`fcitx5/shuangpin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMethod {
    /// `engine` or `engine/inner`, what `switch` takes.
    pub id: String,
    pub name: String,
    pub engine: String,
    pub engine_name: String,
    /// An icon name or file.
    pub icon: Option<String>,
    /// A short label, such as 双.
    pub label: Option<String>,
}

/// Splits a switch target `engine/inner` into the engine and the input method inside.
pub fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('/') {
        Some((engine, inner)) if !inner.is_empty() => (engine, Some(inner)),
        Some((engine, _)) => (engine, None),
        None => (target, None),
    }
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

impl Request {
    /// The engine method that carries this request, for logs.
    fn method(&self) -> &'static str {
        match self {
            Self::Activate => "Activate",
            Self::Update(_) => "UpdateContext",
            Self::Reset => "Reset",
            Self::Key(..) => "ProcessKey",
        }
    }
}

#[derive(Debug)]
struct InFlight {
    token: u64,
    engine: String,
    method: &'static str,
    deadline: Instant,
    /// The key waiting for the answer, if the request was a key.
    key: Option<KeyRef>,
}

#[derive(Debug)]
struct Context {
    origin: Origin,
    info: ContextInfo,
    /// The engine this field talks to; `None` is direct input.
    engine: Option<String>,
    /// Whether the engine has been sent `Activate` for this field.
    activated: bool,
    queue: VecDeque<Request>,
    in_flight: Option<InFlight>,
    /// When the field started waiting for its engine: when it got focus, when it got
    /// another engine, or when a hosted engine was activated for it.
    waiting_since: Instant,
    /// Keys wait until then for a hosted engine to take them.
    hold_until: Option<Instant>,
}

impl Context {
    /// Whether a key that a hosted engine cannot take yet should wait for it, which it
    /// does for [`HOSTED_WAIT`]. Sets the deadline of the wait.
    fn hold_key(&mut self) -> bool {
        let until = self.waiting_since + HOSTED_WAIT;
        let hold = Instant::now() < until;
        self.hold_until = hold.then_some(until);
        hold
    }
}

/// Callback for changes of what is kept across restarts.
type StateListener = Box<dyn Fn(&State)>;
/// Callback for changes of the current engine or of the status it published.
type StatusListener = Box<dyn Fn(Option<&str>, Option<&str>, Option<&Status>)>;

/// One row of [`Core::engines`].
#[derive(Debug, Clone)]
pub struct EngineSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub state: &'static str,
    /// The icon the manifest names.
    pub icon: Option<String>,
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
    on_state: Option<StateListener>,
    on_status: Option<StatusListener>,
    /// Whether applications keep their own engine (`[apps] remember`).
    remember_apps: bool,
    /// The app id of the focused window, when the compositor reports it.
    focused_app: Option<String>,
    /// The engine last chosen in each application (`None` is direct input).
    app_engines: BTreeMap<String, Option<String>>,
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
        ui_connections: mpsc::UnboundedSender<UiConnection>,
        host_display: Option<String>,
    ) -> Self {
        let enabled = enabled_engines(config, &installed);
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
            on_state: None,
            on_status: None,
            remember_apps: config.apps.remember,
            focused_app: None,
            app_engines: BTreeMap::new(),
        }
    }

    /// Registers a callback for changes of what should survive a restart: the current
    /// and the last engine, and the engines of applications.
    pub fn set_state_listener(&mut self, listener: impl Fn(&State) + 'static) {
        self.on_state = Some(Box::new(listener));
    }

    fn state_changed(&self) {
        if let Some(listener) = &self.on_state {
            let slots = || {
                self.engines
                    .ids()
                    .filter_map(|id| Some((id.to_owned(), self.engines.get(id)?)))
            };
            listener(&State {
                current: self.current.clone(),
                last: self.last_engine.clone(),
                apps: self
                    .app_engines
                    .iter()
                    .map(|(app, engine)| (app.clone(), engine.clone().unwrap_or_default()))
                    .collect(),
                inner: slots()
                    .filter_map(|(id, slot)| Some((id, slot.inner.clone()?)))
                    .collect(),
                methods: slots()
                    .filter(|(_, slot)| !slot.methods.is_empty())
                    .map(|(id, slot)| (id, slot.methods.clone()))
                    .collect(),
            });
        }
    }

    /// Calls `listener` with the current engine, input method and status whenever any
    /// of them changes.
    pub fn set_status_listener(
        &mut self,
        listener: impl Fn(Option<&str>, Option<&str>, Option<&Status>) + 'static,
    ) {
        self.on_status = Some(Box::new(listener));
    }

    fn status_changed(&self) {
        if let Some(listener) = &self.on_status {
            listener(
                self.current(),
                self.current_method().as_deref(),
                self.current_status().as_ref(),
            );
        }
    }

    /// Takes over what was saved when the daemon stopped. The current engine is left to
    /// the daemon, which switches to it once the compositor is ready.
    pub fn restore(&mut self, state: &State) {
        self.last_engine = state.last.clone().filter(|e| self.engines.get(e).is_some());
        self.app_engines = state
            .apps
            .iter()
            .map(|(app, engine)| (app.clone(), (!engine.is_empty()).then(|| engine.clone())))
            .collect();
        for (engine, methods) in &state.methods {
            if let Some(slot) = self.engines.get_mut(engine) {
                slot.methods = methods.clone();
            }
        }
        // A framework starts with its own default; give it back the input method in use.
        for (engine, inner) in &state.inner {
            if let Some(slot) = self.engines.get_mut(engine) {
                slot.wanted_inner = Some(inner.clone());
            }
        }
    }

    /// Whether `id` is installed and enabled.
    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|e| e == id)
    }

    /// Applies a new configuration and set of installed engines.
    pub fn reconfigure(
        &mut self,
        config: &Config,
        installed: Vec<InstalledEngine>,
        out: &mut dyn Frontends,
    ) {
        self.enabled = enabled_engines(config, &installed);
        self.hotkeys = parse_hotkeys(config);
        self.key_timeout = Duration::from_millis(config.engine.key_timeout_ms.max(1));
        self.remember_apps = config.apps.remember;
        let removed = self.engines.update_installed(installed);
        // An engine that was disabled or uninstalled cannot stay in use.
        if self.current.as_ref().is_some_and(|c| !self.is_enabled(c))
            && let Err(e) = self.set_current(None, out)
        {
            warn!("{e:#}");
        }
        if self
            .last_engine
            .as_ref()
            .is_some_and(|l| removed.contains(l))
        {
            self.last_engine = None;
        }
        info!(enabled = self.enabled.join(", "), "configuration reloaded");
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// What the tray item of the current engine shows, for hosted engines that have one.
    pub fn current_tray(&self) -> Option<&sni_watcher::HostedTray> {
        self.current
            .as_deref()
            .and_then(|id| self.engines.get(id))
            .and_then(|slot| slot.tray.as_ref())
    }

    /// The status of the current engine: what it published, and for a hosted framework
    /// the input method in use.
    pub fn current_status(&self) -> Option<Status> {
        let slot = self
            .current
            .as_deref()
            .and_then(|id| self.engines.get(id))?;
        let mut status = slot.status.clone();
        if let Some(method) = slot
            .wanted_inner
            .as_deref()
            .or(slot.inner.as_deref())
            .and_then(|inner| slot.methods.iter().find(|m| m.id == inner))
        {
            let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
            status.tooltip = Some(method.name.clone());
            status.label = text(&method.label).or(status.label);
            status.icon = status.icon.or_else(|| text(&method.icon));
        }
        Some(status)
    }

    /// What users choose from: the enabled engines in switching order, each hosted
    /// framework replaced by the input methods inside it once they are known.
    pub fn input_methods(&self) -> Vec<InputMethod> {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let mut methods = Vec::new();
        for id in &self.enabled {
            let Some(slot) = self.engines.get(id) else {
                continue;
            };
            let engine_name = slot.installed.manifest.localized_name(&locale).to_owned();
            if slot.methods.is_empty() {
                methods.push(InputMethod {
                    id: id.clone(),
                    name: engine_name.clone(),
                    engine: id.clone(),
                    engine_name,
                    icon: slot.installed.manifest.engine.icon.clone(),
                    label: None,
                });
                continue;
            }
            for method in &slot.methods {
                methods.push(InputMethod {
                    id: format!("{id}/{}", method.id),
                    name: method.name.clone(),
                    engine: id.clone(),
                    engine_name: engine_name.clone(),
                    icon: text(&method.icon),
                    label: text(&method.label),
                });
            }
        }
        methods
    }

    /// The input method in use: the current engine, or the input method inside it (the
    /// one chosen, while the framework has not taken it yet). `None` is direct input.
    pub fn current_method(&self) -> Option<String> {
        let engine = self.current.as_deref()?;
        let slot = self.engines.get(engine)?;
        let inner = slot.wanted_inner.as_ref().or(slot.inner.as_ref());
        Some(match inner {
            Some(inner) if slot.methods.iter().any(|m| &m.id == inner) => {
                format!("{engine}/{inner}")
            }
            _ => engine.to_owned(),
        })
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
                icon: slot.installed.manifest.engine.icon.clone(),
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
            .filter_map(|c| c.in_flight.as_ref().map(|f| f.deadline).or(c.hold_until));
        requests.chain(self.engines.deadline()).min()
    }

    pub fn on_timer(&mut self, now: Instant, out: &mut dyn Frontends) {
        for failed in self.engines.on_timer(now) {
            self.engine_unavailable(&failed, out);
        }
        let held: Vec<ContextId> = self
            .contexts
            .iter()
            .filter(|(_, c)| c.hold_until.is_some_and(|t| t <= now))
            .map(|(id, _)| *id)
            .collect();
        for ctx in held {
            if let Some(context) = self.contexts.get_mut(&ctx) {
                context.hold_until = None;
            }
            self.pump(ctx, out);
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
                method = flight.method,
                ctx,
                "the engine did not answer in time"
            );
            self.stale_tokens.insert(flight.token);
            if let Some(key) = flight.key {
                out.finish_key(&key, false);
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
            self.pump(ctx, out);
        }
    }

    pub fn on_frontend_event(&mut self, event: FrontendEvent, out: &mut dyn Frontends) {
        match event {
            FrontendEvent::Activated { origin, ctx, info } => {
                let engine = self.current.clone();
                if let Some(id) = &engine
                    && let Err(e) = self.engines.ensure_started(id, false)
                {
                    warn!("{e:#}");
                }
                let mut context = Context {
                    origin,
                    info,
                    engine,
                    activated: false,
                    queue: VecDeque::new(),
                    in_flight: None,
                    waiting_since: Instant::now(),
                    hold_until: None,
                };
                if context.engine.is_some() {
                    context.queue.push_back(Request::Activate);
                }
                self.contexts.insert(ctx, context);
                self.pump(ctx, out);
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
                self.pump(ctx, out);
            }
            FrontendEvent::Deactivated { ctx } => {
                if let Some(context) = self.contexts.remove(&ctx) {
                    if let Some(flight) = context.in_flight {
                        self.stale_tokens.insert(flight.token);
                    }
                    if context.activated {
                        Self::deactivate(&self.engines, context.engine.as_deref(), ctx, out);
                    }
                    // Keys still waiting for a hosted engine go to the application.
                    for request in context.queue {
                        if let Request::Key(key, _) = request {
                            out.finish_key(&key, false);
                        }
                    }
                }
            }
            FrontendEvent::Key { key, event } => {
                let Some(context) = self.contexts.get_mut(&key.ctx()) else {
                    out.finish_key(&key, false);
                    return;
                };
                context.queue.push_back(Request::Key(key, event));
                self.pump(key.ctx(), out);
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
                    self.pump(ctx, out);
                }
            }
            FrontendEvent::FocusedApp(app) => self.on_focused_app(app, out),
            FrontendEvent::Hosted(event) => self.on_hosted(event, out),
        }
    }

    /// Tells `engine` that it no longer serves the field `ctx`.
    fn deactivate(
        engines: &EngineManager,
        engine: Option<&str>,
        ctx: ContextId,
        out: &mut dyn Frontends,
    ) {
        let Some(id) = engine else {
            return;
        };
        let Some(slot) = engines.get(id) else {
            return;
        };
        if slot.installed.manifest.is_hosted() {
            out.hosted_deactivate(&EngineId::from(id), ctx);
        } else if let Some(proxy) = slot.proxy.clone() {
            tokio::task::spawn_local(async move {
                let _ = proxy.deactivate(ctx).await;
            });
        }
    }

    fn on_hosted(&mut self, event: HostedEvent, out: &mut dyn Frontends) {
        match event {
            HostedEvent::Ready { engine } => {
                if !self.engines.hosted_ready(&engine) {
                    return;
                }
                self.timeouts.remove(&*engine);
                // A hosted input method shows its popups when it likes.
                out.set_engine_visible(engine.clone(), true);
                // Fields that waited for the engine, or for its input method on their seat.
                let waiting: Vec<ContextId> = self
                    .contexts
                    .iter()
                    .filter(|(_, c)| c.engine.as_deref() == Some(&*engine) && !c.activated)
                    .map(|(id, _)| *id)
                    .collect();
                for ctx in waiting {
                    if let Some(context) = self.contexts.get_mut(&ctx)
                        && !matches!(context.queue.front(), Some(Request::Activate))
                    {
                        context.queue.push_front(Request::Activate);
                    }
                    self.pump(ctx, out);
                }
            }
            HostedEvent::Grabbed { engine } => {
                let waiting: Vec<ContextId> = self
                    .contexts
                    .iter()
                    .filter(|(_, c)| c.engine.as_deref() == Some(&*engine))
                    .map(|(id, _)| *id)
                    .collect();
                for ctx in waiting {
                    if let Some(context) = self.contexts.get_mut(&ctx) {
                        context.hold_until = None;
                    }
                    self.pump(ctx, out);
                }
            }
            HostedEvent::Actions {
                engine,
                ctx,
                actions,
            } => {
                if let Some(context) = self.contexts.get(&ctx)
                    && context.engine.as_deref() == Some(&*engine)
                {
                    out.apply_actions(context.origin, ctx, &actions);
                }
            }
        }
    }

    /// Another application got focus. With `[apps] remember`, it gets back the engine
    /// last chosen in it; applications without one keep the current engine.
    fn on_focused_app(&mut self, app: Option<String>, out: &mut dyn Frontends) {
        self.focused_app = app;
        if !self.remember_apps {
            return;
        }
        let Some(app) = self.focused_app.clone() else {
            return;
        };
        let Some(engine) = self.app_engines.get(&app).cloned() else {
            return;
        };
        if engine
            .as_deref()
            .is_some_and(|e| !self.is_enabled(split_target(e).0))
        {
            return;
        }
        if let Err(e) = self.set_current(engine, out) {
            warn!(app, "cannot switch to the engine of the application: {e:#}");
        }
    }

    pub async fn on_event(&mut self, event: CoreEvent, out: &mut dyn Frontends) {
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
                    self.pump(ctx, out);
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
                out.set_engine_visible(engine.as_str().into(), false);
                self.engine_unavailable(&engine, out);
                if self.current.as_deref() == Some(&engine) {
                    self.status_changed();
                }
            }
            CoreEvent::HostedTray {
                engine,
                generation,
                tray,
            } => {
                if self.engines.set_tray(&engine, generation, tray) {
                    // The tray item changes when the input method does.
                    self.engines.refresh_inner(&engine);
                    if self.current.as_deref() == Some(&engine) {
                        self.status_changed();
                    }
                }
            }
            CoreEvent::Framework {
                engine,
                generation,
                update,
            } => {
                if self.engines.framework_update(&engine, generation, update) {
                    self.state_changed();
                    self.status_changed();
                }
            }
            CoreEvent::Host { engine, call } => self.on_host_call(&engine, call, out),
            CoreEvent::Reply { ctx, token, reply } => self.on_reply(ctx, token, reply, out),
        }
    }

    fn on_host_call(&mut self, engine: &str, call: HostCall, out: &mut dyn Frontends) {
        match call {
            HostCall::Action { ctx, action } => {
                if let Some(context) = self.contexts.get(&ctx)
                    && context.engine.as_deref() == Some(engine)
                {
                    out.apply_actions(context.origin, ctx, &[action]);
                }
            }
            HostCall::SetPopupVisible(visible) => {
                out.set_engine_visible(engine.into(), visible);
            }
            HostCall::SetStatus(status) => {
                if let Some(slot) = self.engines.get_mut(engine)
                    && slot.status != status
                {
                    slot.status = status;
                    if self.current.as_deref() == Some(engine) {
                        self.status_changed();
                    }
                }
            }
            HostCall::RequestSwitch(target) => {
                let target = (!target.is_empty()).then_some(target);
                if let Err(e) = self.switch_to(target, out) {
                    warn!(engine, "switch requested by engine failed: {e:#}");
                }
            }
        }
    }

    fn on_reply(&mut self, ctx: ContextId, token: u64, reply: Reply, out: &mut dyn Frontends) {
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
                out.clear_preedit(context.origin, ctx);
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
        let origin = context.origin;
        let (handled, actions) = match reply {
            Reply::Actions(Ok(actions)) => (false, actions),
            Reply::Key(Ok((handled, actions))) => (handled, actions),
            Reply::Actions(Err(e)) | Reply::Key(Err(e)) => {
                warn!(
                    engine = flight.engine,
                    method = flight.method,
                    "engine request failed: {e}"
                );
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
        out.apply_actions(origin, ctx, &actions);
        if let Some(key) = flight.key {
            out.finish_key(&key, handled);
        }
        self.pump(ctx, out);
    }

    /// An engine went away (crash, failed start): its fields fall back to direct input
    /// until it is back.
    fn engine_unavailable(&mut self, engine: &str, out: &mut dyn Frontends) {
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
                    out.finish_key(&key, false);
                }
            }
            // Keys pass through until the engine is back; it is activated again then.
            context.activated = false;
            out.clear_preedit(context.origin, ctx);
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            self.pump(ctx, out);
        }
    }

    /// Sends queued requests of `ctx` while nothing is in flight.
    fn pump(&mut self, ctx: ContextId, out: &mut dyn Frontends) {
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
                out.finish_key(&key, true);
                let result = match action {
                    HotkeyAction::Toggle => self.toggle(out),
                    HotkeyAction::Next => self.next(out),
                };
                if let Err(e) = result {
                    warn!("{e:#}");
                }
                continue;
            }

            let slot = context
                .engine
                .as_deref()
                .and_then(|id| self.engines.get(id));
            let hosted = slot.is_some_and(|slot| slot.installed.manifest.is_hosted());
            let state = slot.map(|slot| slot.state);
            match state {
                Some(EngineState::Ready) => {}
                // A hosted engine is activated afresh once it is ready, so only keys
                // matter while it starts; they wait for it only briefly.
                Some(EngineState::Starting { .. }) if hosted => {
                    if !matches!(context.queue.front(), Some(Request::Key(..))) {
                        context.queue.pop_front();
                        continue;
                    }
                    if context.hold_key() {
                        return;
                    }
                    if let Some(Request::Key(key, _)) = context.queue.pop_front() {
                        out.finish_key(&key, false);
                    }
                    continue;
                }
                // Hold requests while the engine starts.
                Some(EngineState::Starting { .. }) => return,
                // No engine, or it is unavailable: keys pass straight through and
                // other requests are dropped (the engine is activated afresh later).
                _ => {
                    if let Some(Request::Key(key, _)) = context.queue.pop_front() {
                        out.finish_key(&key, false);
                    }
                    continue;
                }
            }
            if hosted {
                if !self.pump_hosted(ctx, out) {
                    return;
                }
                continue;
            }
            let Some(request) = context.queue.pop_front() else {
                return;
            };
            self.send(ctx, request);
        }
    }

    /// Hands the next request of `ctx` to its hosted engine, which takes it at once.
    /// Returns `false` if the request has to wait.
    fn pump_hosted(&mut self, ctx: ContextId, out: &mut dyn Frontends) -> bool {
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return false;
        };
        let Some(engine) = context.engine.as_deref().map(EngineId::from) else {
            return false;
        };
        let Some(request) = context.queue.pop_front() else {
            return false;
        };
        match request {
            Request::Activate => {
                context.activated =
                    out.hosted_activate(&engine, context.origin, ctx, &context.info);
                context.waiting_since = Instant::now();
            }
            Request::Update(_) | Request::Reset => {
                let mut reset = matches!(request, Request::Reset);
                if matches!(context.queue.front(), Some(Request::Reset)) {
                    context.queue.pop_front();
                    reset = true;
                }
                if context.activated {
                    out.hosted_update(&engine, ctx, &context.info, reset);
                }
            }
            Request::Key(key, event) => match out.hosted_key(&engine, &key, &event) {
                KeyDelivery::Delivered => context.hold_until = None,
                KeyDelivery::NotReady => {
                    // The input method grabs the keyboard once it learns about the
                    // field; until then, keys wait a moment rather than slip past it.
                    if context.hold_key() {
                        context.queue.push_front(Request::Key(key, event));
                        return false;
                    }
                    out.finish_key(&key, false);
                }
                KeyDelivery::Refused => out.finish_key(&key, false),
            },
        }
        true
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
        let method = request.method();
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
            method,
            deadline: Instant::now() + self.key_timeout,
            key: key_ref,
        });
    }

    /// Switches to `target` (or direct input) because the user or an engine asked for it.
    /// With `[apps] remember`, the focused application keeps this choice.
    pub fn switch_to(
        &mut self,
        target: Option<String>,
        out: &mut dyn Frontends,
    ) -> anyhow::Result<()> {
        self.set_current(target.clone(), out)?;
        if self.remember_apps
            && let Some(app) = self.focused_app.clone()
            && self.app_engines.get(&app) != Some(&target)
        {
            debug!(
                app,
                engine = target.as_deref().unwrap_or("direct"),
                "remembering"
            );
            self.app_engines.insert(app, target);
            self.state_changed();
        }
        Ok(())
    }

    /// Makes `target` (or direct input) the engine of all fields. A target
    /// `engine/inner` also selects the input method `inner` inside a hosted framework.
    pub fn set_current(
        &mut self,
        target: Option<String>,
        out: &mut dyn Frontends,
    ) -> anyhow::Result<()> {
        let (target, inner) = match target.as_deref().map(split_target) {
            Some((engine, inner)) => (Some(engine.to_owned()), inner.map(str::to_owned)),
            None => (None, None),
        };
        if let Some(id) = &target {
            let Some(slot) = self.engines.get(id) else {
                anyhow::bail!("no engine `{id}` is installed");
            };
            if let Some(inner) = &inner {
                anyhow::ensure!(
                    slot.methods.is_empty() || slot.methods.iter().any(|m| &m.id == inner),
                    "`{id}` has no input method `{inner}`"
                );
            }
            // An explicit switch also retries an engine that gave up after crashing.
            self.engines.ensure_started(id, true)?;
            if let Some(inner) = inner {
                self.engines.select_inner(id, inner);
                if target.as_deref() == self.current.as_deref() {
                    self.status_changed();
                }
            }
        }
        if target == self.current {
            return Ok(());
        }
        info!(engine = target.as_deref().unwrap_or("direct"), "switching");
        if let Some(previous) = self.current.take() {
            self.last_engine = Some(previous);
        }
        self.current = target.clone();
        out.set_active_engine(target.as_deref().map(Into::into));

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
            if context.activated {
                Self::deactivate(&self.engines, context.engine.as_deref(), ctx, out);
            }
            context.waiting_since = Instant::now();
            context.hold_until = None;
            out.clear_preedit(context.origin, ctx);
            if let Some(flight) = context.in_flight.take() {
                self.stale_tokens.insert(flight.token);
                if let Some(key) = flight.key {
                    out.finish_key(&key, false);
                }
            }
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            context.activated = false;
            context.engine = target.clone();
            if target.is_some() {
                context.queue.push_front(Request::Activate);
            }
            // Activate the new engine now: a hosted input method takes keys only once
            // it has grabbed the keyboard, which it does when it learns about the field.
            self.pump(ctx, out);
        }
        self.state_changed();
        self.status_changed();
        Ok(())
    }

    /// Switches to the next input method (wrapping around to the first).
    pub fn next(&mut self, out: &mut dyn Frontends) -> anyhow::Result<()> {
        let methods = self.input_methods();
        anyhow::ensure!(!methods.is_empty(), "no engines are enabled");
        let current = self.current_method();
        let position = current
            .as_ref()
            .and_then(|c| methods.iter().position(|m| &m.id == c));
        let next = match position {
            Some(i) => methods[(i + 1) % methods.len()].id.clone(),
            None => methods[0].id.clone(),
        };
        self.switch_to(Some(next), out)
    }

    /// Switches between direct input and the last engine.
    pub fn toggle(&mut self, out: &mut dyn Frontends) -> anyhow::Result<()> {
        if self.current.is_some() {
            return self.switch_to(None, out);
        }
        let target = self
            .last_engine
            .clone()
            .filter(|e| self.enabled.contains(e))
            .or_else(|| self.enabled.first().cloned());
        match target {
            Some(engine) => self.switch_to(Some(engine), out),
            None => anyhow::bail!("no engines are enabled"),
        }
    }

    /// Forgets all text fields, e.g. after the compositor connection was lost.
    pub fn forget_wayland_contexts(&mut self) {
        let (wayland, others) = std::mem::take(&mut self.contexts)
            .into_iter()
            .partition(|(_, context)| matches!(context.origin, Origin::Wayland(_)));
        self.contexts = others;
        for (_, context) in wayland {
            if let Some(flight) = context.in_flight {
                self.stale_tokens.insert(flight.token);
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.engines.shutdown();
    }

    /// Whether engine processes are still running.
    pub fn engines_alive(&self) -> bool {
        self.engines.any_alive()
    }
}

/// The enabled engines in switching order: the configured list (minus what is not
/// installed) or every installed engine.
fn enabled_engines(config: &Config, installed: &[InstalledEngine]) -> Vec<String> {
    let ids: Vec<String> = installed.iter().map(|e| e.id().to_owned()).collect();
    match &config.engines {
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
