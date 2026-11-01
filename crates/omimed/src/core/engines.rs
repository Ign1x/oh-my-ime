//! Installed engines and their processes.
//!
//! For every engine the manager owns two sockets in the engine's runtime directory:
//! `engine` (peer-to-peer D-Bus, the engine protocol) and `wayland` (the UI bridge).
//! It launches the engine process, waits for it to register, keeps the connection and
//! a proxy, and reports everything that happens as [`CoreEvent`]s.
//!
//! Hosted input methods (`protocol = "input-method-v2"`) have only the `wayland`
//! socket: they are ready once they created an input method on the bridge. They run
//! on a private session bus (see [`super::bus`]).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_proto::engine::{Dict, InputMethodInfo, KeyEvent, Property, WireAction};
use omime_proto::manifest::{InstalledEngine, SettingsBus};
use omime_proto::names::{
    DAEMON_BUS_NAME, ENGINE_OBJECT_PATH, ENV_ENGINE_ADDRESS, ENV_ENGINE_ID, ENV_ENGINE_RUNTIME_DIR,
    ENV_HOST_WAYLAND_DISPLAY,
};
use omime_proto::{ApiVersion, engine::Status};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use zbus::{OwnedVariant, Variant};

use super::CoreEvent;
use super::frameworks::{self, FrameworkCommand, FrameworkUpdate, InnerMethod};
use super::sni_watcher::{self, HostedMenu, HostedTray};
use crate::runtime_dir;

/// How long an engine may take to start and register.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Hosted input methods load all their addons first; Fcitx5 may take a while.
const HOSTED_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a hosted input method may take to exit after SIGTERM.
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(2);
/// Restarts after a crash wait this long, doubling up to [`RESTART_MAX`].
const RESTART_MIN: Duration = Duration::from_millis(500);
const RESTART_MAX: Duration = Duration::from_secs(30);
/// More crashes than this within [`CRASH_WINDOW`] and we stop restarting.
const MAX_CRASHES: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(60);

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Engine1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme/Engine"
)]
pub trait Engine {
    fn activate(&self, context: u64, info: &Dict) -> zbus::Result<Vec<WireAction>>;

    #[zbus(no_reply)]
    fn deactivate(&self, context: u64) -> zbus::Result<()>;

    fn update_context(&self, context: u64, changes: &Dict) -> zbus::Result<Vec<WireAction>>;

    fn process_key(
        &self,
        context: u64,
        key: &KeyEvent,
        extras: &Dict,
    ) -> zbus::Result<(bool, Vec<WireAction>)>;

    fn reset(&self, context: u64) -> zbus::Result<Vec<WireAction>>;

    #[zbus(no_reply)]
    fn shutdown(&self) -> zbus::Result<()>;

    /// Since 1.1.
    #[zbus(no_reply)]
    fn select_input_method(&self, id: &str) -> zbus::Result<()>;

    /// Since 1.1.
    #[zbus(no_reply)]
    fn activate_property(&self, id: &str) -> zbus::Result<()>;
}

/// What the engine host object reports to the core.
#[derive(Debug)]
pub enum HostCall {
    Action {
        ctx: u64,
        action: omime_proto::engine::Action,
    },
    SetPopupVisible(bool),
    SetStatus(Status),
    RequestSwitch(String),
    /// The engine's own input methods, and the one in use (protocol 1.1).
    SetInputMethods(Vec<InputMethodInfo>, String),
    SetCurrentInputMethod(String),
    SetProperties(Vec<Property>),
}

/// `io.github.ign1x.OhMyIme.EngineHost1`, served to one engine.
struct HostObject {
    engine: String,
    events: mpsc::UnboundedSender<CoreEvent>,
}

impl HostObject {
    fn send(&self, call: HostCall) {
        // The receiver only disappears while the daemon shuts down.
        let _ = self.events.send(CoreEvent::Host {
            engine: self.engine.clone(),
            call,
        });
    }
}

fn owned(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("values without file descriptors can be owned")
}

#[zbus::interface(name = "io.github.ign1x.OhMyIme.EngineHost1")]
impl HostObject {
    fn register(
        &self,
        id: &str,
        api_major: u16,
        api_minor: u16,
        _capabilities: Dict,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<Dict> {
        if id != self.engine {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "this socket belongs to engine `{}`",
                self.engine
            )));
        }
        let ours = ApiVersion::CURRENT;
        let theirs = ApiVersion {
            major: api_major,
            minor: api_minor,
        };
        if !ours.is_compatible_with(theirs) {
            return Err(zbus::fdo::Error::NotSupported(format!(
                "engine protocol {theirs} is not supported (daemon speaks {ours})"
            )));
        }
        let _ = self.events.send(CoreEvent::Registered {
            engine: self.engine.clone(),
            conn: conn.clone(),
            api_minor,
        });
        let mut session = Dict::new();
        session.insert("api-major".into(), owned(Variant::from(ours.major)));
        session.insert("api-minor".into(), owned(Variant::from(ours.minor)));
        session.insert(
            "daemon-version".into(),
            owned(Variant::from(env!("CARGO_PKG_VERSION"))),
        );
        Ok(session)
    }

    fn commit(&self, context: u64, text: String) {
        use omime_proto::engine::Action;
        self.send(HostCall::Action {
            ctx: context,
            action: Action::Commit(text),
        });
    }

    fn set_preedit(&self, context: u64, text: String, cursor_begin: i32, cursor_end: i32) {
        use omime_proto::engine::{Action, Preedit};
        self.send(HostCall::Action {
            ctx: context,
            action: Action::Preedit(Preedit {
                text,
                cursor_begin,
                cursor_end,
            }),
        });
    }

    fn delete_surrounding(&self, context: u64, before: u32, after: u32) {
        use omime_proto::engine::Action;
        self.send(HostCall::Action {
            ctx: context,
            action: Action::DeleteSurrounding { before, after },
        });
    }

    fn forward_key(&self, context: u64, keycode: u32, pressed: bool) {
        use omime_proto::engine::Action;
        self.send(HostCall::Action {
            ctx: context,
            action: Action::ForwardKey { keycode, pressed },
        });
    }

    fn set_popup_visible(&self, visible: bool) {
        self.send(HostCall::SetPopupVisible(visible));
    }

    fn set_status(&self, status: Dict) {
        self.send(HostCall::SetStatus(Status::from_dict(&status)));
    }

    fn request_switch(&self, engine: String) {
        self.send(HostCall::RequestSwitch(engine));
    }

    fn set_input_methods(&self, methods: Vec<Dict>, current: String) {
        let methods = methods
            .iter()
            .filter_map(InputMethodInfo::from_dict)
            .collect();
        self.send(HostCall::SetInputMethods(methods, current));
    }

    fn set_current_input_method(&self, id: String) {
        self.send(HostCall::SetCurrentInputMethod(id));
    }

    fn set_properties(&self, properties: Vec<Dict>) {
        let properties = properties.iter().filter_map(Property::from_dict).collect();
        self.send(HostCall::SetProperties(properties));
    }
}

/// Lifecycle of an engine process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// Not running.
    Stopped,
    /// Launched, waiting for `Register`.
    Starting { deadline: Instant },
    /// Registered; requests can be sent.
    Ready,
    /// Crashed; restarts at `at`.
    Restarting { at: Instant },
    /// Crashed too often; not restarted until selected again.
    Failed,
}

impl EngineState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting { .. } => "starting",
            Self::Ready => "ready",
            Self::Restarting { .. } => "restarting",
            Self::Failed => "failed",
        }
    }
}

/// A UI connection to the bridge, from the engine `engine`.
#[derive(Debug)]
pub struct UiConnection {
    pub engine: String,
    /// Whether the engine is a hosted input method.
    pub hosted: bool,
    pub socket: OwnedFd,
}

/// One installed engine.
pub struct EngineSlot {
    pub installed: InstalledEngine,
    pub state: EngineState,
    pub proxy: Option<EngineProxy<'static>>,
    pub status: Status,
    /// What the tray item of a hosted engine shows; its status is made from it.
    pub tray: Option<HostedTray>,
    /// The input methods inside a hosted framework, as it last reported them.
    pub methods: Vec<InnerMethod>,
    /// Every input method the framework has, set up or not.
    pub available: Vec<InnerMethod>,
    /// The engine's menu, as it last announced it.
    pub properties: Vec<Property>,
    /// Where clicks on the menu of a hosted engine's tray item go.
    menu_clicks: Option<mpsc::UnboundedSender<i32>>,
    /// The input method the framework uses, as it last reported it.
    pub inner: Option<String>,
    /// The input method chosen through omimed that the framework does not use yet. It is
    /// selected when the adapter runs, and again once the framework reports an input
    /// method in use: Fcitx 5 ignores a selection while no field is focused.
    pub wanted_inner: Option<String>,
    /// Whether `wanted_inner` was selected again after the framework reported another
    /// input method, so that a selection that does not take is not repeated forever.
    wanted_retried: bool,
    /// Commands for the framework's adapter, while it runs.
    framework: Option<mpsc::UnboundedSender<FrameworkCommand>>,
    /// Increases with every launch; events of older processes are ignored.
    generation: u64,
    /// The minor engine protocol version the running engine registered with.
    api_minor: u16,
    /// Stops the process; dropping it stops the process politely.
    kill: Option<oneshot::Sender<Stop>>,
    /// The process is stopped on purpose, to start again at once: its exit is not a
    /// crash.
    reloading: bool,
    crashes: Vec<Instant>,
    sockets_ready: bool,
}

impl std::fmt::Debug for EngineSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineSlot")
            .field("id", &self.installed.id())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// All installed engines.
pub struct EngineManager {
    slots: BTreeMap<String, EngineSlot>,
    events: mpsc::UnboundedSender<CoreEvent>,
    ui_connections: mpsc::UnboundedSender<UiConnection>,
    /// `WAYLAND_DISPLAY` of the real compositor, for engines' non-popup windows.
    host_display: Option<String>,
    /// Engine processes that have not exited yet.
    alive: Rc<Cell<usize>>,
}

impl std::fmt::Debug for EngineManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineManager")
            .field("engines", &self.slots.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl EngineManager {
    pub fn new(
        installed: Vec<InstalledEngine>,
        events: mpsc::UnboundedSender<CoreEvent>,
        ui_connections: mpsc::UnboundedSender<UiConnection>,
        host_display: Option<String>,
    ) -> Self {
        let slots = installed
            .into_iter()
            .map(|installed| (installed.id().to_owned(), Self::slot(installed)))
            .collect();
        Self {
            slots,
            events,
            ui_connections,
            host_display,
            alive: Rc::default(),
        }
    }

    fn slot(installed: InstalledEngine) -> EngineSlot {
        EngineSlot {
            installed,
            state: EngineState::Stopped,
            proxy: None,
            status: Status::default(),
            tray: None,
            methods: Vec::new(),
            available: Vec::new(),
            properties: Vec::new(),
            menu_clicks: None,
            inner: None,
            wanted_inner: None,
            wanted_retried: false,
            framework: None,
            generation: 0,
            api_minor: 0,
            kill: None,
            reloading: false,
            crashes: Vec::new(),
            sockets_ready: false,
        }
    }

    /// Replaces the set of installed engines. Running engines keep running with their
    /// old command until restarted; removed engines are stopped. Returns their ids.
    pub fn update_installed(&mut self, installed: Vec<InstalledEngine>) -> Vec<String> {
        let mut fresh: BTreeMap<String, InstalledEngine> = installed
            .into_iter()
            .map(|e| (e.id().to_owned(), e))
            .collect();
        let removed: Vec<String> = self
            .slots
            .keys()
            .filter(|id| !fresh.contains_key(*id))
            .cloned()
            .collect();
        for id in &removed {
            if let Some(mut slot) = self.slots.remove(id) {
                info!(engine = id, "engine uninstalled");
                if let Some(proxy) = slot.proxy.take() {
                    tokio::task::spawn_local(async move {
                        let _ = proxy.shutdown().await;
                    });
                }
                // Dropping the kill sender lets the supervisor kill the process.
                slot.kill.take();
            }
        }
        for (id, slot) in &mut self.slots {
            if let Some(installed) = fresh.remove(id) {
                slot.installed = installed;
            }
        }
        for (id, installed) in fresh {
            info!(engine = id, "engine installed");
            self.slots.insert(id, Self::slot(installed));
        }
        removed
    }

    pub fn get(&self, id: &str) -> Option<&EngineSlot> {
        self.slots.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut EngineSlot> {
        self.slots.get_mut(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.slots.keys().map(String::as_str)
    }

    /// The earliest startup deadline or restart time.
    pub fn deadline(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(|slot| match slot.state {
                EngineState::Starting { deadline } => Some(deadline),
                EngineState::Restarting { at } => Some(at),
                _ => None,
            })
            .min()
    }

    /// Handles startup timeouts and due restarts. Returns engines that failed to start.
    pub fn on_timer(&mut self, now: Instant) -> Vec<String> {
        let mut failed = Vec::new();
        let due: Vec<String> = self
            .slots
            .iter()
            .filter(|(_, slot)| match slot.state {
                EngineState::Starting { deadline } => deadline <= now,
                EngineState::Restarting { at } => at <= now,
                _ => false,
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let Some(slot) = self.slots.get_mut(&id) else {
                continue;
            };
            match slot.state {
                EngineState::Starting { .. } => {
                    warn!(engine = id, "the engine did not register in time");
                    if let Some(kill) = slot.kill.take() {
                        let _ = kill.send(Stop::Politely);
                    }
                    slot.state = EngineState::Failed;
                    failed.push(id);
                }
                EngineState::Restarting { .. } => {
                    slot.state = EngineState::Stopped;
                    if let Err(e) = self.start(&id) {
                        warn!(engine = id, "restart failed: {e:#}");
                        failed.push(id);
                    }
                }
                _ => {}
            }
        }
        failed
    }

    /// Launches the engine unless it is running or about to. An engine that gave up
    /// after crashing is only retried if `retry_failed` is set.
    pub fn ensure_started(&mut self, id: &str, retry_failed: bool) -> anyhow::Result<()> {
        match self.slots.get_mut(id) {
            Some(slot) if slot.state == EngineState::Stopped => self.start(id),
            Some(slot) if slot.state == EngineState::Failed && retry_failed => {
                slot.crashes.clear();
                self.start(id)
            }
            Some(_) => Ok(()),
            None => anyhow::bail!("no engine `{id}` is installed"),
        }
    }

    fn start(&mut self, id: &str) -> anyhow::Result<()> {
        let events = self.events.clone();
        let ui = self.ui_connections.clone();
        let host_display = self.host_display.clone();
        let Some(slot) = self.slots.get_mut(id) else {
            anyhow::bail!("no engine `{id}` is installed");
        };
        let hosted = slot.installed.manifest.is_hosted();
        if !slot.sockets_ready {
            if !hosted {
                listen_engine(id, events.clone())?;
            }
            listen_ui(id, hosted, ui)?;
            slot.sockets_ready = true;
        }
        let dir = runtime_dir::engine_dir(id)?;
        let argv = slot.installed.command();
        let (program, args) = argv.split_first().context("the engine has no command")?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .env(ENV_ENGINE_ID, id)
            .env("WAYLAND_DISPLAY", runtime_dir::engine_wayland_display(id))
            .env("GDK_BACKEND", "wayland")
            .env("QT_QPA_PLATFORM", "wayland")
            // Without a working bridge connection, toolkits must fail instead of
            // silently opening X11 windows; and they must not load another IM.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("GTK_IM_MODULE")
            .env_remove("QT_IM_MODULE")
            .env_remove("XMODIFIERS")
            .stdin(Stdio::null())
            // Into omimed's log, under the engine's id.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        exit_with_omimed(&mut command);
        if !hosted {
            command.env(
                ENV_ENGINE_ADDRESS,
                format!("unix:path={}", dir.join("engine").display()),
            );
        }
        match &host_display {
            Some(display) => command.env(ENV_HOST_WAYLAND_DISPLAY, display),
            None => command.env_remove(ENV_HOST_WAYLAND_DISPLAY),
        };

        slot.generation += 1;
        let generation = slot.generation;
        let (kill_tx, kill_rx) = oneshot::channel();
        slot.kill = Some(kill_tx);
        slot.proxy = None;
        let timeout = if hosted {
            HOSTED_STARTUP_TIMEOUT
        } else {
            STARTUP_TIMEOUT
        };
        slot.state = EngineState::Starting {
            deadline: Instant::now() + timeout,
        };
        let engine = id.to_owned();
        if hosted {
            // The private bus has to be up before the input method starts, which takes
            // a moment; a missing program is better reported right away.
            let Some(path) = find_program(program) else {
                slot.state = EngineState::Stopped;
                slot.kill = None;
                anyhow::bail!("cannot launch `{program}` for engine `{id}`: it is not installed");
            };
            tokio::task::spawn_local(supervise_hosted(
                engine,
                generation,
                command,
                dir,
                icon_dir(&path),
                kill_rx,
                events,
                Alive::new(&self.alive),
            ));
            return Ok(());
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                slot.state = EngineState::Stopped;
                slot.kill = None;
                return Err(e)
                    .with_context(|| format!("cannot launch `{program}` for engine `{id}`"));
            }
        };
        info!(
            engine = id,
            pid = child.id().unwrap_or(0),
            "engine launched"
        );
        log_output(id, &mut child);
        let alive = Alive::new(&self.alive);
        tokio::task::spawn_local(async move {
            let _alive = alive;
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = events.send(CoreEvent::Exited {
                engine,
                generation,
                status: status.ok(),
            });
        });
        Ok(())
    }

    /// A hosted input method created an input method on the bridge. Returns whether it
    /// may serve text fields: it was started by us and has not exited since.
    pub fn hosted_ready(&mut self, id: &str) -> bool {
        let Some(slot) = self.slots.get_mut(id) else {
            return false;
        };
        match slot.state {
            EngineState::Starting { .. } => {
                slot.state = EngineState::Ready;
                info!(engine = id, "engine ready");
                true
            }
            EngineState::Ready => true,
            _ => false,
        }
    }

    /// The engine registered on `conn`.
    pub async fn registered(
        &mut self,
        id: &str,
        conn: zbus::Connection,
        api_minor: u16,
    ) -> anyhow::Result<()> {
        let proxy = EngineProxy::builder(&conn)
            .destination(DAEMON_BUS_NAME)
            .path(ENGINE_OBJECT_PATH)
            .build()
            .await?;
        let Some(slot) = self.slots.get_mut(id) else {
            return Ok(());
        };
        slot.proxy = Some(proxy);
        slot.api_minor = api_minor;
        slot.state = EngineState::Ready;
        info!(engine = id, "engine ready");
        // An engine with input methods of its own gets back the one in use.
        if let Some(wanted) = slot.wanted_inner.clone() {
            Self::select_native(slot, wanted);
        }
        Ok(())
    }

    /// Asks an engine of omimed's to use its input method `inner` (protocol 1.1).
    fn select_native(slot: &EngineSlot, inner: String) {
        if slot.api_minor < 1 {
            return;
        }
        if let Some(proxy) = slot.proxy.clone() {
            tokio::task::spawn_local(async move {
                let _ = proxy.select_input_method(&inner).await;
            });
        }
    }

    /// Asks the engine `id` to activate its property `property`.
    pub fn activate_property(&self, id: &str, property: &str) -> anyhow::Result<()> {
        let slot = self
            .slots
            .get(id)
            .with_context(|| format!("no engine `{id}`"))?;
        anyhow::ensure!(
            slot.properties.iter().any(|p| p.id == property),
            "engine `{id}` has no property `{property}`"
        );
        if let Some(clicks) = &slot.menu_clicks {
            let entry = property.parse().context("not an entry of the menu")?;
            clicks.send(entry).ok().context("the engine has no menu")?;
            return Ok(());
        }
        let proxy = slot.proxy.clone().context("the engine does not run")?;
        let property = property.to_owned();
        tokio::task::spawn_local(async move {
            let _ = proxy.activate_property(&property).await;
        });
        Ok(())
    }

    /// Takes what an engine of omimed's says about its own input methods, as framework
    /// adapters report them. Returns whether anything changed.
    pub fn native_update(&mut self, id: &str, update: FrameworkUpdate) -> bool {
        let Some(generation) = self.slots.get(id).map(|slot| slot.generation) else {
            return false;
        };
        self.framework_update(id, generation, update)
    }

    /// Takes the menu of the hosted engine `id`'s tray item (launch `generation`) as the
    /// engine's menu. Returns whether it changed.
    pub fn set_hosted_menu(&mut self, id: &str, generation: u64, menu: Option<HostedMenu>) -> bool {
        let Some(slot) = self.slots.get_mut(id) else {
            return false;
        };
        if generation != slot.generation {
            return false;
        }
        let (properties, clicks) = match menu {
            Some(menu) => (menu.properties, Some(menu.clicks)),
            None => (Vec::new(), None),
        };
        slot.menu_clicks = clicks;
        if slot.properties == properties {
            return false;
        }
        slot.properties = properties;
        true
    }

    /// Takes what the tray item of the hosted engine `id` (launch `generation`) shows,
    /// and makes the engine's status from it. Returns whether anything changed.
    pub fn set_tray(&mut self, id: &str, generation: u64, tray: Option<HostedTray>) -> bool {
        let Some(slot) = self.slots.get_mut(id) else {
            return false;
        };
        if generation != slot.generation || slot.tray == tray {
            return false;
        }
        let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        slot.status = tray
            .as_ref()
            .map(|tray| Status {
                label: text(&tray.label),
                icon: text(&tray.icon_name),
                tooltip: text(&tray.title),
            })
            .unwrap_or_default();
        slot.tray = tray;
        true
    }

    /// Takes what the framework adapter of `id` (launch `generation`) reports. Returns
    /// whether the input methods or the current one changed.
    pub fn framework_update(&mut self, id: &str, generation: u64, update: FrameworkUpdate) -> bool {
        let Some(slot) = self.slots.get_mut(id) else {
            return false;
        };
        if generation != slot.generation {
            return false;
        }
        match update {
            FrameworkUpdate::Connected(commands) => {
                if let Some(wanted) = &slot.wanted_inner {
                    let _ = commands.send(FrameworkCommand::Select(wanted.clone()));
                }
                slot.framework = Some(commands);
                false
            }
            FrameworkUpdate::Methods(methods) => {
                let changed = slot.methods != methods;
                slot.methods = methods;
                changed
            }
            FrameworkUpdate::Available(available) => {
                let changed = slot.available != available;
                slot.available = available;
                changed
            }
            FrameworkUpdate::Current(inner) => {
                debug!(
                    engine = id,
                    now = inner.as_deref().unwrap_or("-"),
                    wanted = slot.wanted_inner.as_deref().unwrap_or("-"),
                    retried = slot.wanted_retried,
                    "the framework's input method in use"
                );
                if let (Some(wanted), Some(now)) = (&slot.wanted_inner, &inner) {
                    if wanted == now || slot.wanted_retried {
                        slot.wanted_inner = None;
                    } else if let Some(commands) = &slot.framework {
                        // The framework took no notice before; it uses an input method
                        // now, so it can be asked.
                        let _ = commands.send(FrameworkCommand::Select(wanted.clone()));
                        slot.wanted_retried = true;
                    }
                }
                let changed = slot.inner != inner;
                slot.inner = inner;
                changed
            }
        }
    }

    /// Selects the input method `inner` inside the framework of `id`: now if its adapter
    /// runs, and until the framework reports it in use.
    pub fn select_inner(&mut self, id: &str, inner: String) {
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        match &slot.framework {
            Some(commands) => {
                let _ = commands.send(FrameworkCommand::Select(inner.clone()));
            }
            None => Self::select_native(slot, inner.clone()),
        }
        slot.wanted_inner = Some(inner);
        slot.wanted_retried = false;
    }

    /// Asks the framework adapter of `id` to look at the current input method again.
    pub fn refresh_inner(&self, id: &str) {
        if let Some(commands) = self.slots.get(id).and_then(|s| s.framework.as_ref()) {
            let _ = commands.send(FrameworkCommand::Refresh);
        }
    }

    pub fn exited(&mut self, id: &str, generation: u64, status: Option<ExitStatus>, needed: bool) {
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        if generation != slot.generation {
            return;
        }
        slot.proxy = None;
        slot.kill = None;
        slot.framework = None;
        slot.properties.clear();
        slot.menu_clicks = None;
        // The status of a hosted engine came from its tray item, which is gone.
        if slot.tray.take().is_some() {
            slot.status = Status::default();
        }
        let now = Instant::now();
        if std::mem::take(&mut slot.reloading) {
            debug!(engine = id, "engine stopped to reload");
            slot.state = EngineState::Restarting { at: now };
            return;
        }
        if !needed {
            slot.state = EngineState::Stopped;
            debug!(engine = id, ?status, "engine exited");
            return;
        }
        slot.crashes
            .retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        slot.crashes.push(now);
        if slot.crashes.len() > MAX_CRASHES {
            warn!(engine = id, ?status, "engine keeps crashing; giving up");
            slot.state = EngineState::Failed;
            return;
        }
        let backoff = (RESTART_MIN * 2u32.pow(slot.crashes.len() as u32 - 1)).min(RESTART_MAX);
        warn!(engine = id, ?status, restart_in = ?backoff, "engine exited unexpectedly");
        slot.state = EngineState::Restarting { at: now + backoff };
    }

    /// Kills the engine (it is restarted on demand).
    pub fn restart(&mut self, id: &str) {
        self.stop(id, Stop::Politely);
    }

    /// Kills an engine that does not respond right away, without waiting for it to
    /// save its state; it is restarted on demand.
    pub fn kill_unresponsive(&mut self, id: &str) {
        self.stop(id, Stop::Now);
    }

    /// Stops a running engine and starts it again right away, so that it reads its
    /// settings again. Not a crash.
    pub fn reload(&mut self, id: &str) {
        if let Some(slot) = self.slots.get_mut(id)
            && let Some(kill) = slot.kill.take()
        {
            info!(engine = id, "restarting the engine to read its settings");
            slot.reloading = true;
            let _ = kill.send(Stop::Politely);
        }
    }

    /// Opens the settings program of `id`: an ordinary window on the user's desktop,
    /// with the engine running. Tells the core when it closes, if the engine is to be
    /// restarted then.
    pub fn open_settings(&mut self, id: &str) -> anyhow::Result<()> {
        let slot = self
            .slots
            .get(id)
            .with_context(|| format!("no engine `{id}` is installed"))?;
        let settings = slot
            .installed
            .manifest
            .engine
            .settings
            .clone()
            .with_context(|| format!("engine `{id}` has no settings"))?;
        let argv = slot.installed.settings_command().with_context(|| {
            format!(
                "`{}`, the settings of engine `{id}`, is not installed",
                settings.exec[0]
            )
        })?;
        // The program talks to the engine.
        self.ensure_started(id, true)?;
        let (program, args) = argv.split_first().context("no settings program")?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .env(ENV_ENGINE_ID, id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A window on the real compositor, not an engine window.
        if let Some(display) = &self.host_display {
            command.env("WAYLAND_DISPLAY", display);
        }
        if settings.bus == SettingsBus::Engine {
            let bus = runtime_dir::engine_dir(id)?.join("bus");
            command.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus.display()),
            );
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start `{program}`"))?;
        info!(engine = id, program, "settings opened");
        log_output(id, &mut child);
        let engine = id.to_owned();
        let events = self.events.clone();
        tokio::task::spawn_local(async move {
            let _ = child.wait().await;
            debug!(engine, "settings closed");
            if settings.restart {
                let _ = events.send(CoreEvent::SettingsClosed { engine });
            }
        });
        Ok(())
    }

    /// Whether `id` has a settings program that is installed.
    pub fn has_settings(&self, id: &str) -> bool {
        self.slots
            .get(id)
            .is_some_and(|slot| slot.installed.settings_command().is_some())
    }

    fn stop(&mut self, id: &str, how: Stop) {
        if let Some(slot) = self.slots.get_mut(id)
            && let Some(kill) = slot.kill.take()
        {
            let _ = kill.send(how);
        }
    }

    /// Whether engine processes are still running, for example after [`Self::shutdown`].
    pub fn any_alive(&self) -> bool {
        self.alive.get() > 0
    }

    /// Asks every engine to exit.
    pub fn shutdown(&mut self) {
        for slot in self.slots.values_mut() {
            if let Some(proxy) = slot.proxy.take() {
                tokio::task::spawn_local(async move {
                    let _ = proxy.shutdown().await;
                });
            }
            // Dropping the kill sender lets the supervisor kill the process.
            slot.kill.take();
            slot.state = EngineState::Stopped;
        }
    }
}

/// Serves the engine protocol on the engine's D-Bus socket.
fn listen_engine(engine: &str, events: mpsc::UnboundedSender<CoreEvent>) -> anyhow::Result<()> {
    let path = runtime_dir::engine_dir(engine)?.join("engine");
    let listener = runtime_dir::bind_socket(&path)?;
    let engine = engine.to_owned();
    tokio::task::spawn_local(async move {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    warn!(engine, "accepting an engine connection failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            if !runtime_dir::peer_is_same_user(&stream) {
                warn!(engine, "refusing an engine connection of another user");
                continue;
            }
            let stream = match stream.into_std() {
                Ok(stream) => stream,
                Err(e) => {
                    warn!(engine, "cannot take over an engine connection: {e}");
                    continue;
                }
            };
            let host = HostObject {
                engine: engine.clone(),
                events: events.clone(),
            };
            let engine = engine.clone();
            tokio::task::spawn_local(async move {
                let built = async {
                    zbus::connection::Builder::unix_stream(stream)
                        .server(zbus::Guid::generate())
                        .p2p()
                        .serve_at(omime_proto::names::DAEMON_OBJECT_PATH, host)
                        .build()
                        .await
                };
                match built.await {
                    // The connection lives as long as the engine keeps it open; the core
                    // holds it through the proxy after `Register`.
                    Ok(conn) => conn.closed().await,
                    Err(e) => warn!(engine, "engine connection failed: {e}"),
                }
            });
        }
    });
    Ok(())
}

/// Has the process that `command` starts stopped with SIGTERM when omimed dies, even
/// when it is killed: a child must not outlive the daemon, and nothing else would
/// stop it.
#[allow(unsafe_code)]
pub(crate) fn exit_with_omimed(command: &mut tokio::process::Command) {
    let parent = rustix::process::getpid();
    // SAFETY: the closure runs in the child between fork and exec. It makes three
    // system calls and allocates nothing (errors are plain error numbers).
    unsafe {
        command.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
            // omimed may have died before the child asked to follow it.
            if rustix::process::getppid() != Some(parent) {
                return Err(rustix::io::Errno::SRCH.into());
            }
            Ok(())
        });
    }
}

/// Counts an engine process as running for as long as it exists.
struct Alive(Rc<Cell<usize>>);

impl Alive {
    fn new(count: &Rc<Cell<usize>>) -> Self {
        count.set(count.get() + 1);
        Self(count.clone())
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// Runs a hosted input method: starts its private bus, then the input method, and
/// reports when it exits.
#[allow(clippy::too_many_arguments)]
async fn supervise_hosted(
    engine: String,
    generation: u64,
    mut command: tokio::process::Command,
    dir: PathBuf,
    icon_dir: Option<String>,
    kill: oneshot::Receiver<Stop>,
    events: mpsc::UnboundedSender<CoreEvent>,
    _alive: Alive,
) {
    let exited = |status| {
        let _ = events.send(CoreEvent::Exited {
            engine: engine.clone(),
            generation,
            status,
        });
    };
    // Kept until the input method exits; dropping it stops the bus.
    // The tray watcher goes before the bus, and the bus before the input method.
    let mut _tray = None;
    let mut _framework = None;
    command.env(ENV_ENGINE_RUNTIME_DIR, &dir);
    let _bus = match super::bus::start(&dir).await {
        Ok((bus, address)) => {
            _framework = Some(frameworks::start(
                engine.clone(),
                generation,
                address.clone(),
                dir.clone(),
                events.clone(),
            ));
            match sni_watcher::start(
                engine.clone(),
                generation,
                &address,
                dir.clone(),
                icon_dir,
                events.clone(),
            )
            .await
            {
                Ok(watcher) => _tray = Some(watcher),
                Err(e) => debug!(engine, "no tray item for the hosted input method: {e}"),
            }
            command.env("DBUS_SESSION_BUS_ADDRESS", address);
            Some(bus)
        }
        Err(e) => {
            // A bus that does not exist is better than the user's.
            warn!(
                engine,
                "no private session bus, the engine runs without one: {e:#}"
            );
            command.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", dir.join("no-bus").display()),
            );
            None
        }
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            warn!(engine, "cannot launch the engine: {e}");
            return exited(None);
        }
    };
    info!(engine, pid = child.id().unwrap_or(0), "engine launched");
    log_output(&engine, &mut child);
    let status = tokio::select! {
        status = child.wait() => status,
        how = kill => match how {
            Ok(Stop::Now) => {
                let _ = child.start_kill();
                child.wait().await
            }
            Ok(Stop::Politely) | Err(_) => terminate(&mut child).await,
        },
    };
    exited(status.ok());
}

/// How to stop an engine process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// SIGTERM, and SIGKILL if it does not exit in time, for hosted input methods:
    /// they save their state when they exit. Other engines get SIGKILL.
    Politely,
    /// SIGKILL: the process does not respond anyway.
    Now,
}

/// Writes what an engine process, and whatever it starts, prints to omimed's log, one
/// line at a time under the engine's id (target `engine`: `OMIME_LOG=info,engine=warn`
/// keeps it quiet).
fn log_output(engine: &str, child: &mut tokio::process::Child) {
    let stdout = child
        .stdout
        .take()
        .map(|o| Box::new(o) as Box<dyn AsyncRead + Unpin>);
    let stderr = child
        .stderr
        .take()
        .map(|o| Box::new(o) as Box<dyn AsyncRead + Unpin>);
    for output in [stdout, stderr].into_iter().flatten() {
        tokio::task::spawn_local(log_lines(engine.to_owned(), output));
    }
}

async fn log_lines(engine: String, output: Box<dyn AsyncRead + Unpin>) {
    /// Longer lines are logged in pieces; the engine never waits for omimed.
    const MAX_LINE: u64 = 4096;
    let mut reader = tokio::io::BufReader::new(output);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end();
                if !text.is_empty() {
                    info!(target: "engine", engine, "{text}");
                }
            }
        }
    }
}

/// Stops a process politely: SIGTERM, and SIGKILL if it does not exit in time. Input
/// methods such as Fcitx5 save their state when they exit.
async fn terminate(child: &mut tokio::process::Child) -> std::io::Result<ExitStatus> {
    if let Some(pid) = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        if let Ok(status) = tokio::time::timeout(TERMINATE_TIMEOUT, child.wait()).await {
            return status;
        }
    }
    let _ = child.start_kill();
    child.wait().await
}

/// The icons of the program at `path`: `share/icons` of its installation prefix, if it
/// has one.
fn icon_dir(path: &Path) -> Option<String> {
    let prefix = std::fs::canonicalize(path)
        .ok()?
        .parent()?
        .parent()?
        .to_path_buf();
    let icons = prefix.join("share/icons");
    icons.is_dir().then(|| icons.to_string_lossy().into_owned())
}

/// Where the programs of an engine are installed: the prefixes (`/usr`, a Nix store
/// path) of the program it runs and the programs it requires, found in `$PATH`, as
/// linked there and with the links followed.
pub fn install_prefixes(installed: &InstalledEngine) -> Vec<PathBuf> {
    let engine = &installed.manifest.engine;
    let mut prefixes: Vec<PathBuf> = Vec::new();
    for program in engine.exec.first().into_iter().chain(&engine.requires) {
        let Some(path) = find_program(program) else {
            continue;
        };
        let real = std::fs::canonicalize(&path).ok();
        for path in std::iter::once(path).chain(real) {
            if let Some(prefix) = path.parent().and_then(Path::parent)
                && !prefixes.iter().any(|known| known == prefix)
            {
                prefixes.push(prefix.to_path_buf());
            }
        }
    }
    prefixes
}

/// Where `program` is: itself if it is a path, or the first match in `$PATH`.
fn find_program(program: &str) -> Option<PathBuf> {
    let executable = |path: &Path| {
        rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok() && path.is_file()
    };
    if program.contains('/') {
        let path = PathBuf::from(program);
        return executable(&path).then_some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| executable(path))
}

/// Accepts UI bridge connections on the engine's Wayland socket.
pub fn listen_ui(
    engine: &str,
    hosted: bool,
    connections: mpsc::UnboundedSender<UiConnection>,
) -> anyhow::Result<()> {
    let path = runtime_dir::engine_dir(engine)?.join("wayland");
    let listener = runtime_dir::bind_socket(&path)?;
    let engine = engine.to_owned();
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) if !runtime_dir::peer_is_same_user(&stream) => {
                    warn!(engine, "refusing a UI connection of another user");
                }
                Ok((stream, _)) => match stream.into_std() {
                    Ok(stream) => {
                        let connection = UiConnection {
                            engine: engine.clone(),
                            hosted,
                            socket: OwnedFd::from(stream),
                        };
                        if connections.send(connection).is_err() {
                            break;
                        }
                    }
                    Err(e) => warn!("cannot take over a UI connection: {e}"),
                },
                Err(e) => {
                    warn!(engine, "accepting a UI connection failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok(())
}
