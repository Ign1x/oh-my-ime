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
use omime_proto::engine::{Dict, KeyEvent, WireAction};
use omime_proto::manifest::InstalledEngine;
use omime_proto::names::{
    DAEMON_BUS_NAME, ENGINE_OBJECT_PATH, ENV_ENGINE_ADDRESS, ENV_ENGINE_ID,
    ENV_HOST_WAYLAND_DISPLAY,
};
use omime_proto::{ApiVersion, engine::Status};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use zbus::{OwnedVariant, Variant};

use super::CoreEvent;
use super::sni_watcher::{self, HostedTray};
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
    /// Increases with every launch; events of older processes are ignored.
    generation: u64,
    kill: Option<oneshot::Sender<()>>,
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
            generation: 0,
            kill: None,
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
                        let _ = kill.send(());
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
    pub async fn registered(&mut self, id: &str, conn: zbus::Connection) -> anyhow::Result<()> {
        let proxy = EngineProxy::builder(&conn)
            .destination(DAEMON_BUS_NAME)
            .path(ENGINE_OBJECT_PATH)
            .build()
            .await?;
        let Some(slot) = self.slots.get_mut(id) else {
            return Ok(());
        };
        slot.proxy = Some(proxy);
        slot.state = EngineState::Ready;
        info!(engine = id, "engine ready");
        Ok(())
    }

    /// The engine process exited. If it is `needed`, it is restarted after a backoff.
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

    pub fn exited(&mut self, id: &str, generation: u64, status: Option<ExitStatus>, needed: bool) {
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        if generation != slot.generation {
            return;
        }
        slot.proxy = None;
        slot.kill = None;
        // The status of a hosted engine came from its tray item, which is gone.
        if slot.tray.take().is_some() {
            slot.status = Status::default();
        }
        if !needed {
            slot.state = EngineState::Stopped;
            debug!(engine = id, ?status, "engine exited");
            return;
        }
        let now = Instant::now();
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
        if let Some(slot) = self.slots.get_mut(id)
            && let Some(kill) = slot.kill.take()
        {
            let _ = kill.send(());
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
pub(super) fn exit_with_omimed(command: &mut tokio::process::Command) {
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
    kill: oneshot::Receiver<()>,
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
    let _bus = match super::bus::start(&dir).await {
        Ok((bus, address)) => {
            match sni_watcher::start(
                engine.clone(),
                generation,
                &address,
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
    let status = tokio::select! {
        status = child.wait() => status,
        _ = kill => terminate(&mut child).await,
    };
    exited(status.ok());
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

/// Where `program` is: itself if it is a path, or the first match in `$PATH`.
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
