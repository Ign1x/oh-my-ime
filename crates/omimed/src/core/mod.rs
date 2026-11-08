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

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_proto::config::{Config, HotkeySpec};
use omime_proto::engine::{Action, ContextId, ContextInfo, Dict, KeyEvent, Status, WireAction};
use omime_proto::manifest::InstalledEngine;
use omime_proto::tr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use xkbcommon::xkb;

use crate::catalog::Catalog;
use crate::frontend::{FrontendEvent, Frontends, HostedEvent, KeyDelivery, KeyRef, Origin};
use crate::notify::{EngineFailure, Report, Subject, Trouble};
use crate::state::State;
use crate::wayland::EngineId;
use engines::{EngineManager, EngineState, HostCall, UiConnection};
use frameworks::InnerMethod;

/// How long keys wait for a hosted input method to start, select its method and
/// grab the keyboard once activated, before they pass through.
const HOSTED_WAIT: Duration = Duration::from_secs(1);
/// Activating a field may load a schema or dictionary for the first time. Give
/// that setup at least as long as a hosted method, without extending key deadlines.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-field pending work, in addition to its one in-flight request. Payload
/// accounting uses encoded update dictionaries and key events; barriers use count.
const MAX_QUEUED_REQUESTS: usize = 64;
const MAX_QUEUED_BYTES: usize = 64 * 1024;
/// Deactivate is no-reply, but its send can still block on a stalled transport.
const MAX_DEACTIVATIONS: usize = 64;
/// How often to look whether a hosted input method reads the keys it was given, and
/// for how long it may read nothing before it counts as hung. Keys handed to a hung
/// input method are gone, so it is killed and restarted.
const HOSTED_CHECK: Duration = Duration::from_millis(250);
const HOSTED_HUNG: Duration = Duration::from_secs(3);

/// A hosted engine that was handed keys it has not been seen reading yet.
#[derive(Debug, Clone, Copy)]
struct HostedWatch {
    /// Since when it has read nothing.
    since: Instant,
    next_check: Instant,
    /// What it had not read at the last check.
    unread: usize,
}

/// Events from engines and background tasks, handled on the core's thread.
#[derive(Debug)]
pub enum CoreEvent {
    Registered {
        engine: String,
        conn: zbus::Connection,
        /// The minor engine protocol version it speaks.
        api_minor: u16,
        connection_id: u64,
        input_modes: Vec<String>,
    },
    Exited {
        engine: String,
        generation: u64,
        status: Option<ExitStatus>,
    },
    Host {
        engine: String,
        connection_id: u64,
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
    /// The input methods a hosted framework that never told omimed has, as the files it
    /// installs say.
    Installed {
        engine: String,
        methods: Vec<frameworks::InnerMethod>,
    },
    /// The menu of a hosted engine's tray item changed, or went away.
    HostedMenu {
        engine: String,
        generation: u64,
        menu: Option<sni_watcher::HostedMenu>,
    },
    /// The adapter of a hosted framework (launch `generation`) learnt something.
    Framework {
        engine: String,
        generation: u64,
        update: frameworks::FrameworkUpdate,
    },
    /// The settings program of an engine that reads its settings only when it starts
    /// closed.
    SettingsClosed { engine: String },
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
    /// The language it is for (`zh_CN`, `ja`), when known.
    pub language: Option<String>,
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
    RestoreMode,
    Update(Dict),
    /// Resynchronize from the one retained ContextInfo after overload recovery.
    Refresh,
    Reset,
    Key(KeyRef, KeyEvent, Option<Instant>),
}

impl Request {
    /// The engine method that carries this request, for logs.
    fn method(&self) -> &'static str {
        match self {
            Self::Activate => "Activate",
            Self::RestoreMode => "SetInputMode",
            Self::Update(_) | Self::Refresh => "UpdateContext",
            Self::Reset => "Reset",
            Self::Key(..) => "ProcessKey",
        }
    }
}

/// Owning a call also owns its cancellation, including when the core is dropped.
/// A soft timeout keeps this owner alive until the reply or hard recovery deadline.
#[derive(Debug, Default)]
struct RpcTask(Option<tokio::task::AbortHandle>);

impl RpcTask {
    fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(task.abort_handle()))
    }

    fn is_finished(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(tokio::task::AbortHandle::is_finished)
    }
}

impl Drop for RpcTask {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

#[derive(Debug)]
struct Deactivation {
    engine: String,
    task: RpcTask,
    deadline: Instant,
}

#[derive(Debug)]
struct InFlight {
    _task: RpcTask,
    token: u64,
    engine: String,
    method: &'static str,
    deadline: Instant,
    /// The key waiting for the answer, if the request was a key.
    key: Option<KeyRef>,
    /// Its key has already been returned to the frontend. Until the reply and a
    /// queued reset finish, subsequent keys bypass this engine.
    timed_out: bool,
    app: Option<String>,
    mode: Option<String>,
    /// Last accepted observation before this request was sent. A confirmation
    /// must be newer, even if a later UI action has already overtaken its reply.
    mode_revision: u64,
    input_method: Option<String>,
    epoch: u64,
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
    /// Overload passes keys through until reset and field resynchronization finish.
    recovering: bool,
    /// When the field started waiting for its engine: when it got focus, when it got
    /// another engine, or when a hosted engine was activated for it.
    waiting_since: Instant,
    /// Keys wait until then for a hosted engine to take them.
    hold_until: Option<Instant>,
    /// Whether an asynchronous hosted input context has acknowledged focus.
    framework_ready: bool,
    epoch: u64,
    mode_revision: u64,
    mode_ready: bool,
    mode: Option<(String, String)>,
    /// A restore that may finish asynchronously, for example after deployment.
    pending_mode: Option<(String, String)>,
}

impl Context {
    fn update(&mut self, changes: Dict) {
        if changes.is_empty() {
            return;
        }
        // Never look past a key, reset or restore: those requests observe the
        // preceding field state. Explicit clear values also replace older values.
        if let Some(Request::Update(previous)) = self.queue.back_mut() {
            previous.extend(changes);
        } else {
            self.queue.push_back(Request::Update(changes));
        }
    }

    fn queued_key_deadline(&self) -> Option<Instant> {
        self.queue
            .iter()
            .filter_map(|request| match request {
                Request::Key(_, _, deadline) => *deadline,
                _ => None,
            })
            .min()
    }

    fn queued_payload_bytes(&self) -> usize {
        self.queue
            .iter()
            .filter_map(|request| match request {
                Request::Update(changes) => Some(
                    zbus::wire::serialized_size(
                        zbus::wire::serialized::Context::new(zbus::wire::LE, 0),
                        changes,
                    )
                    .map_or(usize::MAX, |size| size.size()),
                ),
                Request::Key(_, event, _) => Some(
                    zbus::wire::serialized_size(
                        zbus::wire::serialized::Context::new(zbus::wire::LE, 0),
                        event,
                    )
                    .map_or(usize::MAX, |size| size.size()),
                ),
                _ => None,
            })
            .fold(0, usize::saturating_add)
    }

    fn queue_overloaded(&self) -> bool {
        self.queue.len() > MAX_QUEUED_REQUESTS || self.queued_payload_bytes() > MAX_QUEUED_BYTES
    }

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
/// Callback for changes of what the frameworks have.
type CatalogListener = Box<dyn Fn(&Catalog)>;
/// Callback for changes of the engines and input methods to choose from.
type ListsListener = Box<dyn Fn()>;
/// Callback for changes of the current engine's menu.
type PropertiesListener = Box<dyn Fn()>;
/// Callback for changes of the current engine or of the status it published.
type StatusListener = Box<dyn Fn(Option<&str>, Option<&str>, Option<&Status>)>;
type SwitchedListener = Box<dyn Fn(Option<&str>, SwitchReason)>;
/// Callback for trouble to tell the user about, and for its end.
type TroubleListener = Box<dyn Fn(Report)>;

/// Who switched the input method, as the control API's `Switched` says: the switcher
/// overlay shows after the user's own switches, not after a menu's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchReason {
    /// omimed's hotkeys.
    Hotkey,
    /// `Switch`, `Next` or `Toggle` of the control API, as `omime switch` calls them.
    Command,
    /// omimed's tray item and its menu.
    Menu,
    /// An engine asked for it (`RequestSwitch`).
    Engine,
}

impl SwitchReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hotkey => "hotkey",
            Self::Command => "command",
            Self::Menu => "menu",
            Self::Engine => "engine",
        }
    }
}

/// One row of [`Core::engines`].
#[derive(Debug, Clone)]
pub struct EngineSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub state: &'static str,
    /// The icon the manifest names.
    pub icon: Option<String>,
    /// Whether it has a settings program that is installed.
    pub settings: bool,
    pub description: Option<String>,
    /// The languages it is for (BCP 47).
    pub languages: Vec<String>,
    /// Whether it is a hosted Wayland input method rather than an engine of omimed's.
    pub hosted: bool,
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
    /// Bounded cleanup sends for contexts that have already retired.
    deactivations: Vec<Deactivation>,
    /// Consecutive timeouts per engine.
    timeouts: BTreeMap<String, u32>,
    /// Hosted engines that may not have read the keys they were given yet.
    hosted_watch: BTreeMap<String, HostedWatch>,
    /// Engines that want their windows shown, for a new compositor connection.
    visible_engines: BTreeSet<String>,
    /// Called whenever `current` changes.
    on_state: Option<StateListener>,
    on_status: Option<StatusListener>,
    on_switched: Option<SwitchedListener>,
    on_catalog: Option<CatalogListener>,
    on_lists: Option<ListsListener>,
    on_properties: Option<PropertiesListener>,
    on_trouble: Option<TroubleListener>,
    /// Whether applications keep their own engine (`[apps] remember`).
    remember_apps: bool,
    remember_mode: bool,
    app_modes: BTreeMap<String, BTreeMap<String, String>>,
    mode_error: Option<String>,
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
        let enabled = enabled_entries(config, &installed);
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
            deactivations: Vec::new(),
            timeouts: BTreeMap::new(),
            hosted_watch: BTreeMap::new(),
            visible_engines: BTreeSet::new(),
            on_state: None,
            on_status: None,
            on_switched: None,
            on_catalog: None,
            on_lists: None,
            on_properties: None,
            on_trouble: None,
            remember_apps: config.apps.remember,
            remember_mode: config.apps.remember_mode,
            app_modes: BTreeMap::new(),
            mode_error: None,
            focused_app: None,
            app_engines: BTreeMap::new(),
        }
    }

    /// Registers a callback for changes of what should survive a restart: the current
    /// and the last engine, and the engines of applications.
    pub fn set_state_listener(&mut self, listener: impl Fn(&State) + 'static) {
        self.on_state = Some(Box::new(listener));
    }

    pub fn set_catalog_listener(&mut self, listener: impl Fn(&Catalog) + 'static) {
        self.on_catalog = Some(Box::new(listener));
    }

    /// Called when the engines or input methods to choose from may have changed.
    pub fn set_lists_listener(&mut self, listener: impl Fn() + 'static) {
        self.on_lists = Some(Box::new(listener));
    }

    fn lists_changed(&self) {
        if let Some(listener) = &self.on_lists {
            listener();
        }
    }

    /// Called when the menu of the current engine may have changed.
    pub fn set_properties_listener(&mut self, listener: impl Fn() + 'static) {
        self.on_properties = Some(Box::new(listener));
    }

    fn properties_changed(&self) {
        if let Some(listener) = &self.on_properties {
            listener();
        }
    }

    /// Called with trouble the user should hear about (an engine stopped, another input
    /// method holds a seat), and when it is over.
    pub fn set_trouble_listener(&mut self, listener: impl Fn(Report) + 'static) {
        self.on_trouble = Some(Box::new(listener));
    }

    fn report(&self, report: Report) {
        if let Some(listener) = &self.on_trouble {
            listener(report);
        }
    }

    /// Reports that the engine `id` stopped and stays stopped.
    fn engine_failed(&self, id: &str, why: EngineFailure) {
        let locale = omime_proto::i18n::preferences().join(":");
        let name = self.engines.get(id).map_or_else(
            || id.to_owned(),
            |slot| slot.installed.manifest.localized_name(&locale).to_owned(),
        );
        self.report(Report::Trouble(Trouble::Engine {
            id: id.to_owned(),
            name,
            why,
        }));
    }

    /// The menu of the current engine: actions, switches and choices it offers.
    pub fn properties(&self) -> Vec<omime_proto::engine::Property> {
        self.current
            .as_deref()
            .and_then(|id| self.engines.get(id))
            .map(|slot| slot.properties.clone())
            .unwrap_or_default()
    }

    /// Activates the property `id` of the current engine.
    pub fn activate_property(&self, id: &str) -> anyhow::Result<()> {
        let engine = self
            .current
            .as_deref()
            .context(tr!("no engine is in use"))?;
        self.engines.activate_property(engine, id)
    }

    /// A tray click belongs to the engine whose menu the tray host displayed.
    /// Check it here, in the same actor turn that dispatches the activation.
    pub fn activate_engine_property(&self, engine: &str, id: &str) -> anyhow::Result<()> {
        if self.current.as_deref() != Some(engine) {
            debug!(engine, "{}", tr!("ignored stale engine menu click"));
            return Ok(());
        }
        self.engines.activate_property(engine, id)
    }

    /// What the frameworks had when omimed last saw them.
    pub fn restore_catalog(&mut self, catalog: &Catalog) {
        for (engine, available) in &catalog.available {
            if let Some(slot) = self.engines.get_mut(engine) {
                slot.available = available.clone();
            }
        }
    }

    /// Reads in the background which input methods the hosted frameworks have that have
    /// told omimed nothing yet, never having run, from the files they install.
    pub fn read_installed_catalogs(&self) {
        for id in self.engines.ids() {
            let Some(slot) = self.engines.get(id) else {
                continue;
            };
            if !slot.installed.manifest.is_hosted() || !slot.available.is_empty() {
                continue;
            }
            let prefixes = engines::install_prefixes(&slot.installed);
            let engine = id.to_owned();
            let events = self.events.clone();
            tokio::task::spawn_local(async move {
                let methods = crate::catalog::installed(&prefixes).await;
                debug!(
                    engine,
                    count = methods.len(),
                    "input methods in the framework's files"
                );
                if !methods.is_empty() {
                    let _ = events.send(CoreEvent::Installed { engine, methods });
                }
            });
        }
    }

    fn catalog_changed(&self) {
        if let Some(listener) = &self.on_catalog {
            listener(&Catalog {
                available: self
                    .engines
                    .ids()
                    .filter_map(|id| Some((id, self.engines.get(id)?)))
                    .filter(|(_, slot)| !slot.available.is_empty())
                    .map(|(id, slot)| (id.to_owned(), slot.available.clone()))
                    .collect(),
            });
        }
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
                app_modes: self.app_modes.clone(),
                // What the user chose, also before the framework took it.
                inner: slots()
                    .filter_map(|(id, slot)| {
                        Some((id, slot.wanted_inner.clone().or(slot.inner.clone())?))
                    })
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

    /// Calls `listener` with the input method in use and the reason whenever someone
    /// switched it: not when the focus moves between applications with `[apps]
    /// remember`, nor when a hosted framework switches on its own.
    pub fn set_switched_listener(
        &mut self,
        listener: impl Fn(Option<&str>, SwitchReason) + 'static,
    ) {
        self.on_switched = Some(Box::new(listener));
    }

    fn switched(&self, reason: SwitchReason) {
        if let Some(listener) = &self.on_switched {
            listener(self.current_method().as_deref(), reason);
        }
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
        self.app_modes = state.app_modes.clone();
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

    /// Whether the engine `id` is installed and enabled, or input methods inside it are.
    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|e| split_target(e).0 == id)
    }

    /// The engines to run, in switching order: those the enabled entries name.
    fn enabled_engines(&self) -> Vec<&str> {
        let mut engines: Vec<&str> = Vec::new();
        for entry in &self.enabled {
            let engine = split_target(entry).0;
            if !engines.contains(&engine) {
                engines.push(engine);
            }
        }
        engines
    }

    /// Applies a new configuration and set of installed engines.
    pub fn reconfigure(
        &mut self,
        config: &Config,
        installed: Vec<InstalledEngine>,
        out: &mut dyn Frontends,
    ) {
        self.enabled = enabled_entries(config, &installed);
        self.hotkeys = parse_hotkeys(config);
        self.key_timeout = Duration::from_millis(config.engine.key_timeout_ms.max(1));
        self.remember_apps = config.apps.remember;
        self.remember_mode = config.apps.remember_mode;
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
        let ids: Vec<_> = self.contexts.keys().copied().collect();
        for ctx in ids {
            self.remember_confirmed_mode(ctx);
        }
        info!(enabled = self.enabled.join(", "), "configuration reloaded");
        self.lists_changed();
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    pub fn choice_context(&self) -> (String, String) {
        (
            self.focused_app.clone().unwrap_or_default(),
            self.current_method().unwrap_or_default(),
        )
    }

    pub fn choose_input_method(
        &mut self,
        method: String,
        app: String,
        out: &mut dyn Frontends,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            method.is_empty() || self.input_methods().iter().any(|m| m.id == method),
            "{}",
            tr!("the selected input method is no longer enabled")
        );
        // A layer surface can make the focused app unknown. It must never apply a
        // choice to a different application that gained focus in the meantime.
        anyhow::ensure!(
            self.focused_app
                .as_deref()
                .is_none_or(|focused| focused == app),
            "{}",
            tr!("the focused application changed; open the chooser again")
        );
        let target = (!method.is_empty()).then_some(method);
        self.set_current(target.clone(), out)?;
        if self.remember_apps && !app.is_empty() {
            self.app_engines.insert(app, target);
            self.state_changed();
        }
        self.switched(SwitchReason::Menu);
        Ok(())
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
        if slot.mode_method != self.current_method() {
            status.input_mode = None;
        }
        if self.current.as_deref() == Some("rime") {
            match status.input_mode.as_deref() {
                Some("latin") => status.label = Some("A".into()),
                Some("native") => status.label = Some("中".into()),
                _ => {}
            }
        }
        if let Some(method) = slot
            .wanted_inner
            .as_deref()
            .or(slot.inner.as_deref())
            .and_then(|inner| slot.methods.iter().find(|m| m.id == inner))
        {
            let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
            status.tooltip = Some(method.name.clone());
            // The engine's own label says more (Rime's A for English letters), except
            // while a switch is on its way and it still has the old input method's.
            status.label = if slot.wanted_inner.is_some() {
                text(&method.label).or(status.label)
            } else {
                status.label.or_else(|| text(&method.label))
            };
            status.icon = status.icon.or_else(|| text(&method.icon));
        }
        Some(status)
    }

    /// What users choose from, in switching order: the enabled entries, with an engine
    /// that is a hosted framework replaced by the input methods inside it once they are
    /// known, and an entry `engine/inner` standing for that input method alone.
    pub fn input_methods(&self) -> Vec<InputMethod> {
        let locale = omime_proto::i18n::preferences().join(":");
        let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let mut methods: Vec<InputMethod> = Vec::new();
        for entry in &self.enabled {
            let (id, inner) = split_target(entry);
            let Some(slot) = self.engines.get(id) else {
                continue;
            };
            let engine_name = slot.installed.manifest.localized_name(&locale).to_owned();
            let inside = |method: &InnerMethod| InputMethod {
                id: format!("{id}/{}", method.id),
                name: method.name.clone(),
                engine: id.to_owned(),
                engine_name: engine_name.clone(),
                icon: text(&method.icon),
                label: text(&method.label),
                language: text(&method.language),
            };
            match inner {
                // Named before the framework has listed it, it is still there to choose,
                // under the name the framework's files give it if they have it.
                Some(inner) => {
                    let method = slot
                        .methods
                        .iter()
                        .chain(&slot.available)
                        .find(|m| m.id == inner);
                    methods.push(inside(method.unwrap_or(&InnerMethod {
                        id: inner.to_owned(),
                        name: inner.to_owned(),
                        ..InnerMethod::default()
                    })));
                }
                None if slot.methods.is_empty() => methods.push(InputMethod {
                    id: id.to_owned(),
                    name: engine_name.clone(),
                    engine: id.to_owned(),
                    engine_name: engine_name.clone(),
                    icon: slot.installed.manifest.engine.icon.clone(),
                    label: None,
                    language: slot.installed.manifest.engine.languages.first().cloned(),
                }),
                None => methods.extend(slot.methods.iter().map(inside)),
            }
        }
        // `fcitx5` and `fcitx5/shuangpin` both name 双拼: the first place counts.
        let mut seen = HashSet::new();
        methods.retain(|m| seen.insert(m.id.clone()));
        methods
    }

    /// Every input method there is to choose from, and whether it is enabled: the
    /// enabled ones in switching order, then what the installed engines have besides
    /// (input methods of frameworks that are not set up or enabled, engines that are
    /// not enabled).
    pub fn all_input_methods(&self) -> Vec<(InputMethod, bool)> {
        let locale = omime_proto::i18n::preferences().join(":");
        let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let mut all: Vec<(InputMethod, bool)> = self
            .input_methods()
            .into_iter()
            .map(|m| (m, true))
            .collect();
        for id in self.engines.ids() {
            let Some(slot) = self.engines.get(id) else {
                continue;
            };
            let engine_name = slot.installed.manifest.localized_name(&locale).to_owned();
            let mut offered: Vec<InputMethod> = slot
                .methods
                .iter()
                .chain(&slot.available)
                .map(|method| InputMethod {
                    id: format!("{id}/{}", method.id),
                    name: method.name.clone(),
                    engine: id.to_owned(),
                    engine_name: engine_name.clone(),
                    icon: text(&method.icon),
                    label: text(&method.label),
                    language: text(&method.language),
                })
                .collect();
            if offered.is_empty() {
                offered.push(InputMethod {
                    id: id.to_owned(),
                    name: engine_name.clone(),
                    engine: id.to_owned(),
                    engine_name,
                    icon: slot.installed.manifest.engine.icon.clone(),
                    label: None,
                    language: slot.installed.manifest.engine.languages.first().cloned(),
                });
            }
            for method in offered {
                if !all.iter().any(|(m, _)| m.id == method.id) {
                    all.push((method, false));
                }
            }
        }
        all
    }

    /// The input method in use: the current engine, or the input method inside it (the
    /// one chosen, while the framework has not taken it yet). `None` is direct input.
    pub fn current_method(&self) -> Option<String> {
        let engine = self.current.as_deref()?;
        let slot = self.engines.get(engine)?;
        let inner = slot.wanted_inner.as_ref().or(slot.inner.as_ref());
        // One the configuration names counts before the framework lists it.
        let known = |inner: &str| {
            slot.methods.iter().any(|m| m.id == inner)
                || self
                    .enabled
                    .iter()
                    .any(|e| split_target(e) == (engine, Some(inner)))
        };
        Some(match inner {
            Some(inner) if known(inner) => format!("{engine}/{inner}"),
            _ => engine.to_owned(),
        })
    }

    /// Whether the selected method has been configured. Hosted frameworks may
    /// still wait for focus before activating it; keys wait for that confirmation.
    pub fn input_method_configured(&self) -> bool {
        let Some(slot) = self.current.as_deref().and_then(|id| self.engines.get(id)) else {
            return true;
        };
        slot.wanted_inner.as_ref().is_none_or(|wanted| {
            slot.selection
                .as_ref()
                .is_some_and(|(id, result)| id == wanted && result.is_ok())
        })
    }

    pub fn input_method_error(&self) -> Option<&str> {
        let slot = self
            .current
            .as_deref()
            .and_then(|id| self.engines.get(id))?;
        let (id, result) = slot.selection.as_ref()?;
        (slot.wanted_inner.as_ref() == Some(id))
            .then(|| result.as_ref().err().map(String::as_str))
            .flatten()
    }

    /// All installed engines, enabled ones first in switching order.
    pub fn engines(&self) -> Vec<EngineSummary> {
        let locale = omime_proto::i18n::preferences().join(":");
        let summary = |id: &str, enabled: bool| {
            self.engines.get(id).map(|slot| EngineSummary {
                id: id.to_owned(),
                name: slot.installed.manifest.localized_name(&locale).to_owned(),
                enabled,
                state: slot.state.name(),
                icon: slot.installed.manifest.engine.icon.clone(),
                settings: self.engines.has_settings(id),
                description: slot
                    .installed
                    .manifest
                    .localized_description(&locale)
                    .map(str::to_owned),
                languages: slot.installed.manifest.engine.languages.clone(),
                hosted: slot.installed.manifest.is_hosted(),
            })
        };
        let enabled_ids = self.enabled_engines();
        let enabled = enabled_ids.iter().filter_map(|id| summary(id, true));
        let disabled = self
            .engines
            .ids()
            .filter(|id| !enabled_ids.contains(id))
            .filter_map(|id| summary(id, false));
        enabled.chain(disabled).collect()
    }

    pub fn deadline(&self) -> Option<Instant> {
        let requests = self
            .contexts
            .values()
            .flat_map(|c| {
                [
                    c.in_flight.as_ref().map(|f| f.deadline),
                    c.hold_until,
                    c.queued_key_deadline(),
                ]
            })
            .flatten();
        let watches = self.hosted_watch.values().map(|w| w.next_check);
        let retiring = self.deactivations.iter().map(|call| call.deadline);
        requests
            .chain(watches)
            .chain(retiring)
            .chain(self.engines.deadline())
            .min()
    }

    pub fn on_timer(&mut self, now: Instant, out: &mut dyn Frontends) {
        let previous_mode_error = self.mode_error.clone();
        for (failed, why) in self.engines.on_timer(now) {
            self.engine_failed(&failed, why);
            self.engine_unavailable(&failed, out);
        }
        self.check_hosted(now, out);
        self.deactivations.retain(|call| !call.task.is_finished());
        let stalled: BTreeSet<_> = self
            .deactivations
            .iter()
            .filter(|call| call.deadline <= now)
            .map(|call| call.engine.clone())
            .collect();
        for engine in stalled {
            warn!(
                engine,
                "the engine transport did not finish deactivation; restarting it"
            );
            self.engines.kill_unresponsive(&engine);
            self.engine_unavailable(&engine, out);
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
            let Some(flight) = context.in_flight.as_mut() else {
                continue;
            };
            if flight.timed_out {
                let engine = flight.engine.clone();
                warn!(
                    engine,
                    "the engine did not recover after a timeout; restarting it"
                );
                self.engines.kill_unresponsive(&engine);
                self.engine_unavailable(&engine, out);
                continue;
            }
            warn!(
                engine = flight.engine,
                method = flight.method,
                ctx,
                "the engine did not answer in time"
            );
            flight.timed_out = true;
            if let Some(mode) = &flight.mode {
                self.mode_error = Some(tr!(
                    "{engine} timed out restoring input mode {mode}",
                    engine = flight.engine,
                    mode = mode
                ));
            }
            // Keep the request in flight: Reset must follow its completion, and
            // must finish before any new composition reaches the engine.
            flight.deadline = now + self.key_timeout * 2;
            if let Some(key) = flight.key.take() {
                out.finish_key(&key, false);
            }
            out.clear_preedit(context.origin, ctx);
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
        let queued: Vec<_> = self
            .contexts
            .iter()
            .filter(|(_, c)| {
                c.queued_key_deadline()
                    .is_some_and(|deadline| deadline <= now)
            })
            .map(|(id, _)| *id)
            .collect();
        for ctx in queued {
            self.recover_queue(ctx, now, out);
            self.pump(ctx, out);
        }
        if self.mode_error != previous_mode_error {
            self.status_changed();
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
                    self.engine_failed(id, EngineFailure::Launch(format!("{e:#}")));
                }
                let mut context = Context {
                    origin,
                    info,
                    engine,
                    activated: false,
                    queue: VecDeque::new(),
                    in_flight: None,
                    recovering: false,
                    waiting_since: Instant::now(),
                    hold_until: None,
                    framework_ready: true,
                    epoch: 0,
                    mode_revision: 0,
                    mode_ready: false,
                    mode: None,
                    pending_mode: None,
                };
                if context.engine.is_some() {
                    context.queue.push_back(Request::Activate);
                    context.queue.push_back(Request::RestoreMode);
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
                let app_changed = info.app_id != context.info.app_id;
                let identifying_initial_app =
                    context.epoch == 0 && context.info.app_id.is_none() && info.app_id.is_some();
                let changes = info.diff(&context.info);
                context.info = info;
                if app_changed {
                    context.epoch += 1;
                    // Foreign-toplevel can identify a newly focused field after
                    // Activate was sent. Its initial mode still belongs to this
                    // field; later app changes must continue to stale the reply.
                    if identifying_initial_app
                        && let Some(flight) = context.in_flight.as_mut()
                        && flight.method == "Activate"
                        && flight.epoch == 0
                        && flight.app.is_none()
                    {
                        flight.app = context.info.app_id.clone();
                        flight.epoch = context.epoch;
                    }
                    context.mode_ready = false;
                    context.mode = None;
                    context.pending_mode = None;
                }
                if context.engine.is_some() {
                    context.update(changes);
                    if reset {
                        context.queue.push_back(Request::Reset);
                    }
                    if app_changed {
                        context.queue.push_back(Request::RestoreMode);
                    }
                }
                self.pump(ctx, out);
            }
            FrontendEvent::Deactivated { ctx } => {
                if let Some(context) = self.contexts.remove(&ctx) {
                    if let Some(flight) = context.in_flight
                        && let Some(key) = flight.key
                    {
                        out.finish_key(&key, false);
                    }
                    if context.activated {
                        self.deactivate(context.engine.as_deref(), ctx, out);
                    }
                    // Keys still waiting for a hosted engine go to the application.
                    for request in context.queue {
                        if let Request::Key(key, _, _) = request {
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
                let native = context
                    .engine
                    .as_deref()
                    .and_then(|id| self.engines.get(id))
                    .is_some_and(|slot| !slot.installed.manifest.is_hosted());
                let deadline = native.then(|| {
                    let ordinary = Instant::now() + self.key_timeout;
                    // Initial dictionary/schema loading keeps its existing allowance.
                    // It is tied to activation, never extended by arriving keys.
                    if !context.activated
                        || context
                            .in_flight
                            .as_ref()
                            .is_some_and(|f| f.method == "Activate")
                    {
                        ordinary
                            .max(context.waiting_since + self.key_timeout.max(ACTIVATION_TIMEOUT))
                    } else {
                        ordinary
                    }
                });
                context.queue.push_back(Request::Key(key, event, deadline));
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
                    context.update(changes);
                    self.pump(ctx, out);
                }
            }
            FrontendEvent::FocusedApp(app) => self.on_focused_app(app, out),
            FrontendEvent::Hosted(event) => self.on_hosted(event, out),
            FrontendEvent::SeatTaken { seat, taken: true } => {
                self.report(Report::Trouble(Trouble::Seat { name: seat }));
            }
            FrontendEvent::SeatTaken { seat, taken: false } => {
                self.report(Report::Resolved(Subject::Seat(seat)));
            }
        }
    }

    /// Tells `engine` that it no longer serves the field `ctx`.
    fn deactivate(&mut self, engine: Option<&str>, ctx: ContextId, out: &mut dyn Frontends) {
        let Some(id) = engine else {
            return;
        };
        let Some(slot) = self.engines.get(id) else {
            return;
        };
        if slot.installed.manifest.is_hosted() {
            out.hosted_deactivate(&EngineId::from(id), ctx);
        } else if !self.deactivate_native(id, ctx) {
            self.engine_unavailable(id, out);
        }
    }

    fn admit_deactivation(&mut self, engine: &str) -> bool {
        self.deactivations.retain(|call| !call.task.is_finished());
        if self.deactivations.len() >= MAX_DEACTIVATIONS {
            // Dropping a cleanup send alone could strand state in the engine.
            // Retire that engine instead; its exit clears all its contexts.
            warn!(
                engine,
                "too many pending deactivations; restarting the engine"
            );
            self.engines.kill_unresponsive(engine);
            self.deactivations.retain(|call| call.engine != engine);
            return false;
        }
        true
    }

    fn deactivate_native(&mut self, engine: &str, ctx: ContextId) -> bool {
        let Some(proxy) = self.engines.get(engine).and_then(|slot| slot.proxy.clone()) else {
            return true;
        };
        if !self.admit_deactivation(engine) {
            return false;
        }
        self.deactivations.push(Deactivation {
            engine: engine.to_owned(),
            deadline: Instant::now() + self.key_timeout,
            task: RpcTask::new(tokio::task::spawn_local(async move {
                let _ = proxy.deactivate(ctx).await;
            })),
        });
        true
    }

    fn on_hosted(&mut self, event: HostedEvent, out: &mut dyn Frontends) {
        match event {
            HostedEvent::Ready { engine } => {
                if !self.engines.hosted_ready(&engine) {
                    return;
                }
                self.report(Report::Resolved(Subject::Engine(engine.to_string())));
                self.timeouts.remove(&*engine);
                // A hosted input method shows its popups when it likes.
                self.set_engine_visible(&engine, true, out);
                // Fields that waited for the engine, or for its input method on their seat.
                let waiting: Vec<ContextId> = self
                    .contexts
                    .iter()
                    .filter(|(_, c)| c.engine.as_deref() == Some(&*engine) && !c.activated)
                    .map(|(id, _)| *id)
                    .collect();
                for ctx in waiting {
                    if let Some(context) = self.contexts.get_mut(&ctx)
                        && !context.queue.iter().any(|r| matches!(r, Request::Activate))
                    {
                        context.queue.push_front(Request::RestoreMode);
                        context.queue.push_front(Request::Activate);
                    }
                    self.pump(ctx, out);
                }
            }
            HostedEvent::Grabbed { engine } => {
                // The framework has focused the field now.
                self.engines.reselect_inner(&engine);
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
                    out.apply_actions(context.origin, ctx, &actions, None);
                }
            }
            HostedEvent::ForwardKey { engine, ctx, key } => {
                if let Some(context) = self.contexts.get(&ctx)
                    && context.engine.as_deref() == Some(&*engine)
                {
                    out.forward_key(context.origin, ctx, &key);
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
            CoreEvent::Registered {
                engine,
                conn,
                api_minor,
                connection_id,
                input_modes,
            } => {
                if let Err(e) = self
                    .engines
                    .registered(&engine, conn, api_minor, connection_id, input_modes)
                    .await
                {
                    warn!(engine, "cannot talk to the engine: {e}");
                    return;
                }
                self.report(Report::Resolved(Subject::Engine(engine.clone())));
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
                        && !context.queue.iter().any(|r| matches!(r, Request::Activate))
                    {
                        context.queue.push_front(Request::RestoreMode);
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
                let was_failed = self
                    .engines
                    .get(&engine)
                    .is_some_and(|s| s.state == EngineState::Failed);
                if !self.engines.exited(&engine, generation, status, needed) {
                    return;
                }
                if !was_failed
                    && self
                        .engines
                        .get(&engine)
                        .is_some_and(|s| s.state == EngineState::Failed)
                {
                    self.engine_failed(&engine, EngineFailure::Crashed);
                }
                // A new process has to ask for its window again.
                self.set_engine_visible(&engine, false, out);
                self.engine_unavailable(&engine, out);
                if self.current.as_deref() == Some(&engine) {
                    self.status_changed();
                    // Its menu went with it.
                    self.properties_changed();
                }
            }
            CoreEvent::Installed { engine, methods } => {
                // What the framework itself said wins.
                if let Some(slot) = self.engines.get_mut(&engine)
                    && slot.available.is_empty()
                {
                    slot.available = methods;
                    self.lists_changed();
                    self.catalog_changed();
                }
            }
            CoreEvent::HostedMenu {
                engine,
                generation,
                menu,
            } => {
                if self.engines.set_hosted_menu(&engine, generation, menu)
                    && self.current.as_deref() == Some(&engine)
                {
                    self.properties_changed();
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
                let catalog = matches!(update, frameworks::FrameworkUpdate::Available(_));
                let methods = matches!(update, frameworks::FrameworkUpdate::Methods(_));
                let needs_focus = matches!(
                    update,
                    frameworks::FrameworkUpdate::Connected {
                        needs_focus: true,
                        ..
                    }
                );
                let focused = match update {
                    frameworks::FrameworkUpdate::Focused(ctx) => Some(ctx),
                    _ => None,
                };
                if self.engines.framework_update(&engine, generation, update) {
                    if let Some(context) = focused.and_then(|ctx| self.contexts.get_mut(&ctx))
                        && context.engine.as_deref() == Some(&engine)
                    {
                        context.framework_ready = true;
                    }
                    if catalog {
                        self.catalog_changed();
                    } else {
                        self.state_changed();
                        self.status_changed();
                    }
                    if catalog || methods {
                        self.lists_changed();
                    }
                    // A hosted framework can grab the keyboard before it has
                    // selected the requested input method. Release the first keys
                    // as soon as that selection is confirmed.
                    let waiting: Vec<ContextId> = self
                        .contexts
                        .iter()
                        .filter(|(_, c)| c.engine.as_deref() == Some(&engine))
                        .map(|(id, _)| *id)
                        .collect();
                    for ctx in waiting {
                        if needs_focus && let Some(context) = self.contexts.get_mut(&ctx) {
                            // Detection may finish after the first activation. Give
                            // the adapter a fresh focus transition to observe.
                            if context.activated {
                                out.hosted_deactivate(&EngineId::from(engine.as_str()), ctx);
                                context.activated = false;
                                context.queue.push_front(Request::Activate);
                            }
                        }
                        self.pump(ctx, out);
                    }
                }
            }
            CoreEvent::Host {
                engine,
                connection_id,
                call,
            } => {
                if self.engines.get(&engine).is_some_and(|s| {
                    s.connection_id == Some(connection_id) && matches!(s.state, EngineState::Ready)
                }) {
                    self.on_host_call(&engine, call, out);
                }
            }
            CoreEvent::Reply { ctx, token, reply } => self.on_reply(ctx, token, reply, out),
            // It reads what was changed when it starts again.
            CoreEvent::SettingsClosed { engine } => {
                if self.engines.reload(&engine) {
                    self.engine_unavailable(&engine, out);
                }
            }
        }
    }

    fn on_host_call(&mut self, engine: &str, call: HostCall, out: &mut dyn Frontends) {
        match call {
            HostCall::Action { ctx, action } => {
                if let Some(context) = self.contexts.get(&ctx)
                    && context.engine.as_deref() == Some(engine)
                {
                    out.apply_actions(context.origin, ctx, &[action], None);
                }
            }
            HostCall::SetPopupVisible(visible) => {
                self.set_engine_visible(engine, visible, out);
            }
            HostCall::InputMode {
                ctx,
                input_method,
                mode,
                revision,
            } => {
                // Revisions order UI actions against pending key replies. During
                // restoration a newer UI report may update status, but is not saved
                // until the ordered restoration completes.
                if self.contexts.get(&ctx).is_some_and(|c| {
                    c.activated && c.in_flight.as_ref().is_none_or(|f| !f.timed_out)
                }) {
                    self.accept_mode(engine, ctx, input_method, mode, revision);
                }
            }
            HostCall::SetStatus(mut status) => {
                // Only context-bound confirmations may change the machine-readable mode.
                status.input_mode = self
                    .engines
                    .get(engine)
                    .and_then(|s| s.status.input_mode.clone());
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
                if let Err(e) = self.switch_to(target, SwitchReason::Engine, out) {
                    warn!(engine, "switch requested by engine failed: {e:#}");
                }
            }
            HostCall::SetInputMethods(methods, current) => {
                let methods = methods
                    .into_iter()
                    .filter(|m| !m.id.contains('/'))
                    .map(|m| InnerMethod {
                        id: m.id,
                        name: m.name,
                        icon: m.icon.unwrap_or_default(),
                        label: m.label.unwrap_or_default(),
                        language: m.language.unwrap_or_default(),
                    })
                    .collect();
                let listed = self
                    .engines
                    .native_update(engine, frameworks::FrameworkUpdate::Methods(methods));
                let current = (!current.is_empty()).then_some(current);
                let used = self
                    .engines
                    .native_update(engine, frameworks::FrameworkUpdate::Current(current));
                self.pump_engine_contexts(engine, out);
                if listed || used {
                    self.state_changed();
                    self.status_changed();
                }
                if listed {
                    self.lists_changed();
                }
            }
            HostCall::SetProperties(properties) => {
                if let Some(slot) = self.engines.get_mut(engine)
                    && slot.properties != properties
                {
                    slot.properties = properties;
                    if self.current.as_deref() == Some(engine) {
                        self.properties_changed();
                    }
                }
            }
            HostCall::SetCurrentInputMethod(id) => {
                let current = (!id.is_empty()).then_some(id);
                if self
                    .engines
                    .native_update(engine, frameworks::FrameworkUpdate::Current(current))
                {
                    self.state_changed();
                    self.status_changed();
                }
                self.pump_engine_contexts(engine, out);
            }
        }
    }

    fn pump_engine_contexts(&mut self, engine: &str, out: &mut dyn Frontends) {
        let ids: Vec<_> = self
            .contexts
            .iter()
            .filter(|(_, c)| c.engine.as_deref() == Some(engine))
            .map(|(id, _)| *id)
            .collect();
        for ctx in ids {
            self.pump(ctx, out);
        }
    }

    fn on_reply(&mut self, ctx: ContextId, token: u64, reply: Reply, out: &mut dyn Frontends) {
        let current_method = self.current_method();
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return;
        };
        let Some(flight) = context.in_flight.take_if(|f| f.token == token) else {
            return;
        };
        if flight.method == "Reset" && matches!(&reply, Reply::Actions(Err(_))) {
            let engine = flight.engine.clone();
            warn!(engine, ctx, "the engine could not reset; restarting it");
            self.engines.kill_unresponsive(&engine);
            self.engine_unavailable(&engine, out);
            return;
        }
        if flight.timed_out {
            debug!(ctx, "discarding a late answer");
            if flight.method != "Reset" && !matches!(context.queue.front(), Some(Request::Reset)) {
                context.queue.push_front(Request::Reset);
            }
            self.pump(ctx, out);
            return;
        }
        self.timeouts.remove(&flight.engine);
        let origin = context.origin;
        let same_app = context.epoch == flight.epoch
            && context.info.app_id == flight.app
            && current_method == flight.input_method;
        let (handled, actions) = match reply {
            Reply::Actions(Ok(actions)) => (false, actions),
            Reply::Key(Ok((handled, actions))) => (handled, actions),
            Reply::Actions(Err(e)) | Reply::Key(Err(e)) => {
                warn!(
                    engine = flight.engine,
                    method = flight.method,
                    error = %engines::error_name(&e),
                    "engine request failed"
                );
                debug!(engine = flight.engine, method = flight.method, error = %e,
                    "engine request failure details");
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
        let mode_confirmed = flight.mode.as_ref().is_none_or(|wanted| {
            let reply_confirmation = actions
                .iter()
                .filter_map(|action| {
                    let Action::InputMode {
                        input_method,
                        mode,
                        revision,
                    } = action
                    else {
                        return None;
                    };
                    let target = if input_method.is_empty() {
                        flight.engine.clone()
                    } else {
                        format!("{}/{input_method}", flight.engine)
                    };
                    (current_method.as_deref() == Some(&target) && *revision > flight.mode_revision)
                        .then_some((revision, mode))
                })
                .max_by_key(|(revision, _)| *revision)
                .is_some_and(|(_, mode)| mode == wanted);
            // A deployment/UI thread can report confirmation before the RPC reply.
            let async_confirmation = self.contexts.get(&ctx).is_some_and(|context| {
                context.mode_revision > flight.mode_revision
                    && context.mode.as_ref().is_some_and(|(inner, mode)| {
                        let target = if inner.is_empty() {
                            flight.engine.clone()
                        } else {
                            format!("{}/{inner}", flight.engine)
                        };
                        current_method.as_deref() == Some(&target) && mode == wanted
                    })
            });
            reply_confirmation || async_confirmation
        });
        if flight.mode.is_some() && same_app {
            if let Some(context) = self.contexts.get_mut(&ctx) {
                context.mode_ready = mode_confirmed;
                if mode_confirmed {
                    context.pending_mode = None;
                }
            }
            if !mode_confirmed {
                self.set_mode_error(Some(tr!(
                    "{engine} did not confirm input mode {mode}",
                    engine = flight.engine,
                    mode = flight.mode.as_deref().unwrap_or_default()
                )));
            }
        }
        let mut text_actions = Vec::new();
        for action in actions {
            match action {
                Action::InputMode {
                    input_method,
                    mode,
                    revision,
                } => {
                    if same_app {
                        self.accept_mode(&flight.engine, ctx, input_method, mode, revision);
                    }
                }
                other => text_actions.push(other),
            }
        }
        // Text must reach the application before a key it did not consume.
        out.apply_actions(origin, ctx, &text_actions, flight.key.as_ref());
        if flight.mode.is_some() && same_app && mode_confirmed {
            self.set_mode_error(None);
            self.remember_confirmed_mode(ctx);
        }
        if let Some(key) = flight.key {
            out.finish_key(&key, handled);
        }
        self.pump(ctx, out);
    }

    /// An engine went away (crash, failed start): its fields fall back to direct input
    /// until it is back.
    fn engine_unavailable(&mut self, engine: &str, out: &mut dyn Frontends) {
        self.deactivations.retain(|call| call.engine != engine);
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
            if let Some(flight) = context.in_flight.take()
                && let Some(key) = flight.key
            {
                out.finish_key(&key, false);
            }
            // Keys pass through until the engine is back; it is activated again then.
            context.activated = false;
            context.recovering = false;
            context.mode_ready = false;
            context.mode_revision = 0;
            context.mode = None;
            context.pending_mode = None;
            out.clear_preedit(context.origin, ctx);
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            self.pump(ctx, out);
        }
    }

    pub fn quarantine_engine(&mut self, engine: &str, out: &mut dyn Frontends) {
        warn!(
            engine,
            "quarantining an engine after a compositor protocol error"
        );
        self.engines.quarantine(engine);
        self.engine_failed(engine, EngineFailure::Protocol);
        self.set_engine_visible(engine, false, out);
        self.engine_unavailable(engine, out);
        self.status_changed();
        self.lists_changed();
    }

    /// Release accepted keys in arrival order and rebuild just the state needed
    /// for recovery. Never cancel a live call here: its late reply must be discarded
    /// before Reset, so it cannot duplicate text or erase a new composition.
    fn recover_queue(&mut self, ctx: ContextId, now: Instant, out: &mut dyn Frontends) {
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return;
        };
        warn!(
            ctx,
            "queued engine work exceeded its size or key arrival deadline"
        );
        context.recovering = true;
        // Reset and Refresh may report the retired composition's mode. Keep those
        // observations visible, but protect the saved target until RestoreMode
        // confirms it. A previous asynchronous restore must not reopen saving.
        context.mode_ready = false;
        context.pending_mode = None;
        if let Some(flight) = context.in_flight.as_mut() {
            if !flight.timed_out {
                flight.timed_out = true;
                flight.deadline = now + self.key_timeout * 2;
            }
            if let Some(key) = flight.key.take() {
                out.finish_key(&key, false);
            }
        }
        for request in context.queue.drain(..) {
            if let Request::Key(key, _, _) = request {
                out.finish_key(&key, false);
            }
        }
        out.clear_preedit(context.origin, ctx);
        if context.engine.is_some() {
            if context.activated {
                // Reset is also queued for an in-flight Reset: later field changes
                // may have made its original snapshot obsolete.
                context.queue.push_back(Request::Reset);
                context.queue.push_back(Request::Refresh);
            } else {
                context.queue.push_back(Request::Activate);
            }
            context.queue.push_back(Request::RestoreMode);
        }
    }

    /// Sends queued requests of `ctx` while nothing is in flight.
    fn pump(&mut self, ctx: ContextId, out: &mut dyn Frontends) {
        loop {
            let now = Instant::now();
            if self.contexts.get(&ctx).is_some_and(|context| {
                context.queue_overloaded()
                    || context
                        .queued_key_deadline()
                        .is_some_and(|deadline| deadline <= now)
            }) {
                self.recover_queue(ctx, now, out);
            }
            let Some(context) = self.contexts.get_mut(&ctx) else {
                return;
            };
            let bypass =
                context.recovering || context.in_flight.as_ref().is_some_and(|f| f.timed_out);
            let waiting_key = bypass
                .then(|| {
                    context
                        .queue
                        .iter()
                        .position(|r| matches!(r, Request::Key(..)))
                })
                .flatten();
            if waiting_key.is_none() && context.in_flight.is_some() {
                return;
            }
            let index = waiting_key.unwrap_or(0);
            let Some(request) = context.queue.get(index) else {
                context.recovering = false;
                return;
            };

            // Hotkeys are handled in order with the keys around them.
            if let Request::Key(key, event, _) = request
                && let Some(action) = match_hotkey(&self.hotkeys, event)
            {
                let key = *key;
                context.queue.remove(index);
                out.finish_key(&key, true);
                let result = match action {
                    HotkeyAction::Toggle => self.toggle(SwitchReason::Hotkey, out),
                    HotkeyAction::Next => self.next(SwitchReason::Hotkey, out),
                };
                if let Err(e) = result {
                    warn!("{e:#}");
                }
                continue;
            }

            if waiting_key.is_some() {
                // Recovery must finish before another composition can start.
                // Until then every new key gets an immediate answer.
                if let Some(Request::Key(key, _, _)) = context.queue.remove(index) {
                    out.finish_key(&key, false);
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
                    if let Some(Request::Key(key, _, _)) = context.queue.pop_front() {
                        out.finish_key(&key, false);
                    }
                    continue;
                }
                // Hold requests while the engine starts.
                Some(EngineState::Starting { .. }) => return,
                // No engine, or it is unavailable: keys pass straight through and
                // other requests are dropped (the engine is activated afresh later).
                _ => {
                    if let Some(Request::Key(key, _, _)) = context.queue.pop_front() {
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
            if matches!(context.queue.front(), Some(Request::RestoreMode))
                && slot.is_some_and(|s| !s.input_modes.is_empty() && s.wanted_inner.is_some())
            {
                if context.hold_key() {
                    return;
                }
                context.queue.pop_front();
                self.set_mode_error(Some(tr!(
                    "input method selection did not finish before mode restoration"
                )));
                continue;
            }
            let Some(request) = context.queue.pop_front() else {
                return;
            };
            self.send(ctx, request);
        }
    }

    /// Kills hosted engines that stopped reading what they are sent: the keys they were
    /// handed are gone, and every key after them would be too.
    fn check_hosted(&mut self, now: Instant, out: &mut dyn Frontends) {
        let due: Vec<String> = self
            .hosted_watch
            .iter()
            .filter(|(_, w)| w.next_check <= now)
            .map(|(engine, _)| engine.clone())
            .collect();
        for engine in due {
            let Some(watch) = self.hosted_watch.get_mut(&engine) else {
                continue;
            };
            let unread = out.hosted_unread(&EngineId::from(engine.as_str()));
            match unread {
                // It read everything, or it is gone anyway.
                None | Some(0) => {
                    self.hosted_watch.remove(&engine);
                }
                Some(unread) => {
                    if unread < watch.unread {
                        // It read something since; more has come.
                        watch.since = now;
                    } else if now.duration_since(watch.since) >= HOSTED_HUNG {
                        warn!(
                            engine,
                            unread,
                            "the hosted input method does not read what it is sent; restarting it"
                        );
                        self.hosted_watch.remove(&engine);
                        self.engines.kill_unresponsive(&engine);
                        continue;
                    }
                    watch.unread = unread;
                    watch.next_check = now + HOSTED_CHECK;
                }
            }
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
                context.framework_ready = !self.engines.focus_inner(&engine, ctx);
                context.activated =
                    out.hosted_activate(&engine, context.origin, ctx, &context.info);
                context.waiting_since = Instant::now();
            }
            Request::RestoreMode => {
                context.mode_ready = true;
            }
            Request::Update(_) | Request::Refresh | Request::Reset => {
                let mut reset = matches!(request, Request::Reset);
                if matches!(context.queue.front(), Some(Request::Reset)) {
                    context.queue.pop_front();
                    reset = true;
                }
                if context.activated {
                    out.hosted_update(&engine, ctx, &context.info, reset);
                }
            }
            Request::Key(key, event, deadline) => {
                let selecting = self
                    .engines
                    .get(&engine)
                    .is_some_and(|slot| slot.wanted_inner.is_some());
                if !context.framework_ready || selecting {
                    if context.hold_key() {
                        context.queue.push_front(Request::Key(key, event, deadline));
                        return false;
                    }
                    // Without its input context the framework would lose the key. One
                    // that is still selecting gets it after the selection, which it was
                    // sent first: switching can take longer than the wait (Fcitx 5
                    // loading pinyin on a busy machine), and the key belongs to the
                    // input method switched to, not to the application.
                    if !context.framework_ready {
                        out.finish_key(&key, false);
                        return true;
                    }
                }
                match out.hosted_key(&engine, &key, &event) {
                    KeyDelivery::Delivered => {
                        context.hold_until = None;
                        let now = Instant::now();
                        self.hosted_watch
                            .entry(engine.to_string())
                            .or_insert(HostedWatch {
                                since: now,
                                next_check: now + HOSTED_CHECK,
                                unread: 0,
                            });
                    }
                    KeyDelivery::NotReady => {
                        // The input method grabs the keyboard once it learns about the
                        // field; until then, keys wait a moment rather than slip past it.
                        if context.hold_key() {
                            context.queue.push_front(Request::Key(key, event, deadline));
                            return false;
                        }
                        out.finish_key(&key, false);
                    }
                    KeyDelivery::Refused => out.finish_key(&key, false),
                }
            }
        }
        true
    }

    /// Accept only a mode tied to the live field, selected method and engine capability.
    fn accept_mode(
        &mut self,
        engine: &str,
        ctx: ContextId,
        inner: String,
        mode: String,
        revision: u64,
    ) {
        let method = if inner.is_empty() {
            engine.to_owned()
        } else {
            format!("{engine}/{inner}")
        };
        if self.current_method().as_deref() != Some(&method)
            || !self
                .engines
                .get(engine)
                .is_some_and(|s| s.input_modes.contains(&mode))
        {
            return;
        }
        let Some(context) = self.contexts.get_mut(&ctx) else {
            return;
        };
        if context.engine.as_deref() != Some(engine) {
            return;
        }
        // A compositor may report the new app before destroying the old context.
        if context.info.app_id.is_some()
            && self.focused_app.is_some()
            && context.info.app_id != self.focused_app
        {
            return;
        }
        if revision <= context.mode_revision {
            return;
        }
        context.mode_revision = revision;
        context.mode = Some((inner.clone(), mode.clone()));
        let recovered = context.in_flight.is_none()
            && context.pending_mode.as_ref() == Some(&(method.clone(), mode.clone()));
        if recovered {
            context.mode_ready = true;
            context.pending_mode = None;
        }

        let status_changed = if let Some(slot) = self.engines.get_mut(engine) {
            let changed = slot.status.input_mode.as_deref() != Some(&mode)
                || slot.mode_method.as_deref() != Some(&method);
            slot.status.input_mode = Some(mode.clone());
            slot.mode_method = Some(method.clone());
            changed
        } else {
            false
        };
        self.remember_confirmed_mode(ctx);
        if recovered {
            self.set_mode_error(None);
        }
        if status_changed {
            self.status_changed();
        }
    }

    fn remember_confirmed_mode(&mut self, ctx: ContextId) {
        if !self.remember_apps || !self.remember_mode {
            return;
        }
        let Some(context) = self.contexts.get(&ctx).filter(|c| c.mode_ready) else {
            return;
        };
        let (Some(app), Some(engine), Some((inner, mode))) =
            (&context.info.app_id, &context.engine, &context.mode)
        else {
            return;
        };
        let method = if inner.is_empty() {
            engine.clone()
        } else {
            format!("{engine}/{inner}")
        };
        if self.current_method().as_deref() != Some(&method) {
            return;
        }
        let modes = self.app_modes.entry(app.clone()).or_default();
        if modes.get(&method) != Some(mode)
            || self.app_engines.get(app) != Some(&Some(method.clone()))
        {
            modes.insert(method.clone(), mode.clone());
            self.app_engines.insert(app.clone(), Some(method));
            self.state_changed();
        }
    }

    pub fn input_mode_error(&self) -> Option<&str> {
        self.mode_error.as_deref()
    }

    fn set_mode_error(&mut self, error: Option<String>) {
        if self.mode_error != error {
            self.mode_error = error;
            self.status_changed();
        }
    }

    fn send(&mut self, ctx: ContextId, request: Request) {
        let wanted_mode = if matches!(request, Request::RestoreMode) {
            let wanted = self.contexts.get(&ctx).and_then(|c| {
                let engine = c.engine.as_deref()?;
                let slot = self.engines.get(engine)?;
                if !self.remember_apps || !self.remember_mode {
                    return None;
                }
                let app = c.info.app_id.as_ref()?;
                let method = self.current_method()?;
                let mode = self.app_modes.get(app)?.get(&method)?;
                slot.input_modes.contains(mode).then(|| mode.clone())
            });
            if wanted.is_none() {
                if let Some(context) = self.contexts.get_mut(&ctx) {
                    context.mode_ready = true;
                }
                self.remember_confirmed_mode(ctx);
                return;
            }
            self.set_mode_error(None);
            wanted
        } else {
            None
        };
        let input_method = self.current_method();
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
        let deadline = match &request {
            Request::Activate => Instant::now() + self.key_timeout.max(ACTIVATION_TIMEOUT),
            Request::Key(_, _, Some(deadline)) => {
                (*deadline).min(Instant::now() + self.key_timeout)
            }
            _ => Instant::now() + self.key_timeout,
        };
        let mut key_ref = None;
        let app = context.info.app_id.clone();
        let epoch = context.epoch;
        let mode = wanted_mode.clone();
        let mode_revision = context.mode_revision;
        if let (Some(method), Some(mode)) = (&input_method, &wanted_mode) {
            context.mode_ready = false;
            context.pending_mode = Some((method.clone(), mode.clone()));
        }
        let request = match request {
            Request::Activate => {
                context.activated = true;
                Request::Activate
            }
            Request::Refresh => Request::Update(context.info.to_dict()),
            other => other,
        };
        let info = matches!(request, Request::Activate).then(|| context.info.to_dict());
        if let Request::Key(key, _, _) = &request {
            key_ref = Some(*key);
        }
        let task = RpcTask::new(tokio::task::spawn_local(async move {
            let reply = match request {
                Request::Activate => Reply::Actions(
                    proxy
                        .activate(
                            ctx,
                            &info.expect("activation captures the field information"),
                        )
                        .await,
                ),
                Request::RestoreMode => Reply::Actions(
                    proxy
                        .set_input_mode(
                            ctx,
                            &wanted_mode.expect("restore without a saved mode returns above"),
                        )
                        .await,
                ),
                Request::Update(changes) => {
                    Reply::Actions(proxy.update_context(ctx, &changes).await)
                }
                Request::Reset => Reply::Actions(proxy.reset(ctx).await),
                Request::Key(_, event, _) => {
                    Reply::Key(proxy.process_key(ctx, &event, &Dict::new()).await)
                }
                Request::Refresh => unreachable!("Refresh was expanded above"),
            };
            let _ = events.send(CoreEvent::Reply { ctx, token, reply });
        }));
        context.in_flight = Some(InFlight {
            _task: task,
            token,
            engine,
            method,
            deadline,
            key: key_ref,
            timed_out: false,
            app,
            mode,
            mode_revision,
            input_method,
            epoch,
        });
    }

    /// Switches to `target` (or direct input) because the user or an engine asked for it.
    /// With `[apps] remember`, the focused application keeps this choice.
    pub fn switch_to(
        &mut self,
        target: Option<String>,
        reason: SwitchReason,
        out: &mut dyn Frontends,
    ) -> anyhow::Result<()> {
        self.set_current(target.clone(), out)?;
        self.switched(reason);
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
                anyhow::bail!("{}", tr!("no engine `{id}` is installed", id = id));
            };
            if let Some(inner) = &inner {
                // One the framework has not listed yet, but the configuration names, is
                // added to the framework when it is selected.
                let named = self
                    .enabled
                    .iter()
                    .any(|e| split_target(e) == (id, Some(inner)));
                anyhow::ensure!(
                    slot.methods.is_empty() || named || slot.methods.iter().any(|m| &m.id == inner),
                    "{}",
                    tr!(
                        "`{id}` has no input method `{inner}`",
                        id = id,
                        inner = inner
                    )
                );
            }
            // An explicit switch also retries an engine that gave up after crashing.
            if let Err(e) = self.engines.ensure_started(id, true) {
                self.engine_failed(id, EngineFailure::Launch(format!("{e:#}")));
                return Err(e);
            }
            if let Some(inner) = inner {
                let changed = self.engines.get(id).is_some_and(|slot| {
                    slot.wanted_inner.as_deref().or(slot.inner.as_deref()) != Some(&inner)
                });
                self.engines.select_inner(id, inner);
                for context in self.contexts.values_mut() {
                    if context.engine.as_deref() == Some(id) {
                        context.waiting_since = Instant::now();
                        if changed {
                            context.epoch += 1;
                            context.mode_ready = false;
                            context.mode = None;
                            context.pending_mode = None;
                            context.queue.retain(|r| !matches!(r, Request::RestoreMode));
                            let after_activation = context
                                .queue
                                .iter()
                                .position(|r| matches!(r, Request::Activate))
                                .map_or(0, |i| i + 1);
                            context.queue.insert(after_activation, Request::RestoreMode);
                        }
                    }
                }
                if changed && let Some(slot) = self.engines.get_mut(id) {
                    slot.status.input_mode = None;
                }
                self.state_changed();
                if target.as_deref() == self.current.as_deref() {
                    self.status_changed();
                }
            }
        }
        if target == self.current {
            let ids: Vec<_> = self.contexts.keys().copied().collect();
            for ctx in ids {
                self.pump(ctx, out);
            }
            return Ok(());
        }
        info!(engine = target.as_deref().unwrap_or("direct"), "switching");
        if let Some(previous) = self.current.take() {
            self.last_engine = Some(previous);
        }
        self.current = target.clone();
        self.mode_error = None;
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
            let old_engine = context.activated.then(|| context.engine.clone()).flatten();
            self.deactivate(old_engine.as_deref(), ctx, out);
            let Some(context) = self.contexts.get_mut(&ctx) else {
                continue;
            };
            context.waiting_since = Instant::now();
            context.hold_until = None;
            out.clear_preedit(context.origin, ctx);
            if let Some(flight) = context.in_flight.take()
                && let Some(key) = flight.key
            {
                out.finish_key(&key, false);
            }
            context.queue.retain(|r| matches!(r, Request::Key(..)));
            context.activated = false;
            context.recovering = false;
            context.mode_ready = false;
            context.mode_revision = 0;
            context.mode = None;
            context.pending_mode = None;
            context.engine = target.clone();
            if target.is_some() {
                context.queue.push_front(Request::RestoreMode);
                context.queue.push_front(Request::Activate);
            }
            // Activate the new engine now: a hosted input method takes keys only once
            // it has grabbed the keyboard, which it does when it learns about the field.
            self.pump(ctx, out);
        }
        self.state_changed();
        self.status_changed();
        // Another engine, another menu.
        self.properties_changed();
        Ok(())
    }

    /// Switches to the next input method (wrapping around to the first).
    pub fn next(&mut self, reason: SwitchReason, out: &mut dyn Frontends) -> anyhow::Result<()> {
        let methods = self.input_methods();
        anyhow::ensure!(!methods.is_empty(), "{}", tr!("no engines are enabled"));
        let current = self.current_method();
        let position = current
            .as_ref()
            .and_then(|c| methods.iter().position(|m| &m.id == c));
        let next = match position {
            Some(i) => methods[(i + 1) % methods.len()].id.clone(),
            None => methods[0].id.clone(),
        };
        self.switch_to(Some(next), reason, out)
    }

    /// Switches between direct input and the last engine.
    pub fn toggle(&mut self, reason: SwitchReason, out: &mut dyn Frontends) -> anyhow::Result<()> {
        if self.current.is_some() {
            return self.switch_to(None, reason, out);
        }
        let target = self
            .last_engine
            .clone()
            .filter(|e| self.is_enabled(split_target(e).0))
            .or_else(|| self.enabled.first().cloned());
        match target {
            Some(engine) => self.switch_to(Some(engine), reason, out),
            None => anyhow::bail!("{}", tr!("no engines are enabled")),
        }
    }

    /// The input method remembered for each application (`None` is direct input).
    pub fn apps(&self) -> Vec<(String, Option<String>)> {
        self.app_engines
            .iter()
            .map(|(app, target)| (app.clone(), target.clone()))
            .collect()
    }

    /// Remembers `target` (`None`: direct input) for `app`, and uses it if `app` has
    /// the focus.
    pub fn set_app(
        &mut self,
        app: String,
        target: Option<String>,
        out: &mut dyn Frontends,
    ) -> anyhow::Result<()> {
        if let Some(target) = &target {
            let engine = split_target(target).0;
            anyhow::ensure!(
                self.engines.get(engine).is_some(),
                "{}",
                tr!("no engine `{engine}` is installed", engine = engine)
            );
        }
        if self.remember_apps && self.focused_app.as_deref() == Some(app.as_str()) {
            self.set_current(target.clone(), out)?;
        }
        self.app_engines.insert(app, target);
        self.state_changed();
        Ok(())
    }

    /// Forgets what `app` used: it gets the input method in use when it has focus.
    pub fn forget_app(&mut self, app: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.app_engines.remove(app).is_some(),
            "{}",
            tr!("nothing is remembered for `{app}`", app = app)
        );
        self.app_modes.remove(app);
        self.state_changed();
        Ok(())
    }

    /// Opens the settings program of `engine`.
    pub fn open_settings(&mut self, engine: &str) -> anyhow::Result<()> {
        self.engines.open_settings(engine)
    }

    /// Forgets all text fields, e.g. after the compositor connection was lost.
    pub fn forget_wayland_contexts(&mut self) {
        let (wayland, others) = std::mem::take(&mut self.contexts)
            .into_iter()
            .partition(|(_, context)| matches!(context.origin, Origin::Wayland(_)));
        self.contexts = others;
        for (ctx, context) in wayland {
            // The disconnected frontend no longer owns its keys. Dropping its
            // in-flight owner still cancels the RPC immediately.
            drop(context.in_flight);
            if context.activated
                && let Some(engine) = context.engine
                && self
                    .engines
                    .get(&engine)
                    .is_some_and(|slot| !slot.installed.manifest.is_hosted())
            {
                self.deactivate_native(&engine, ctx);
            }
        }
    }

    /// Records whether `engine` wants its windows shown, and tells the bridge.
    fn set_engine_visible(&mut self, engine: &str, visible: bool, out: &mut dyn Frontends) {
        if visible {
            self.visible_engines.insert(engine.to_owned());
        } else {
            self.visible_engines.remove(engine);
        }
        out.set_engine_visible(engine.into(), visible);
    }

    /// The engines that want their windows shown, which a new compositor connection
    /// has to learn.
    pub fn visible_engines(&self) -> impl Iterator<Item = &str> {
        self.visible_engines.iter().map(String::as_str)
    }

    pub fn shutdown(&mut self) {
        self.engines.shutdown();
    }

    /// Whether engine processes are still running.
    pub fn engines_alive(&self) -> bool {
        self.engines.any_alive()
    }
}

/// What `config` enables, in switching order: engines (`fcitx5`: all its input
/// methods) and input methods inside them (`fcitx5/shuangpin`). Entries of engines
/// that are not installed are left out. Without a list, every installed engine that is
/// enabled by default: demo engines and other frameworks are only offered.
fn enabled_entries(config: &Config, installed: &[InstalledEngine]) -> Vec<String> {
    let ids: Vec<&str> = installed.iter().map(InstalledEngine::id).collect();
    match &config.engines {
        Some(list) => list
            .iter()
            .filter(|entry| {
                let engine = split_target(entry).0;
                let known = ids.contains(&engine);
                if !known {
                    warn!(engine, "configured engine is not installed");
                }
                known
            })
            .cloned()
            .collect(),
        None => installed
            .iter()
            .filter(|e| e.manifest.engine.enabled_by_default)
            .map(|e| e.id().to_owned())
            .collect(),
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

#[cfg(test)]
mod mode_tests;
