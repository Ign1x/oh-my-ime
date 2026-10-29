//! Installed engines and their processes.
//!
//! For every engine the manager owns two sockets in the engine's runtime directory:
//! `engine` (peer-to-peer D-Bus, the engine protocol) and `wayland` (the UI bridge).
//! It launches the engine process, waits for it to register, keeps the connection and
//! a proxy, and reports everything that happens as [`CoreEvent`]s.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::process::{ExitStatus, Stdio};
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
use crate::runtime_dir;

/// How long an engine may take to start and register.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
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

/// One installed engine.
pub struct EngineSlot {
    pub installed: InstalledEngine,
    pub state: EngineState,
    pub proxy: Option<EngineProxy<'static>>,
    pub status: Status,
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
    ui_connections: mpsc::UnboundedSender<(String, OwnedFd)>,
    /// `WAYLAND_DISPLAY` of the real compositor, for engines' non-popup windows.
    host_display: Option<String>,
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
        ui_connections: mpsc::UnboundedSender<(String, OwnedFd)>,
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
        }
    }

    fn slot(installed: InstalledEngine) -> EngineSlot {
        EngineSlot {
            installed,
            state: EngineState::Stopped,
            proxy: None,
            status: Status::default(),
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
        if !slot.sockets_ready {
            listen_engine(id, events.clone())?;
            listen_ui(id, ui)?;
            slot.sockets_ready = true;
        }
        let dir = runtime_dir::engine_dir(id)?;
        let argv = slot.installed.command();
        let (program, args) = argv.split_first().context("the engine has no command")?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .env(ENV_ENGINE_ID, id)
            .env(
                ENV_ENGINE_ADDRESS,
                format!("unix:path={}", dir.join("engine").display()),
            )
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
        match &host_display {
            Some(display) => command.env(ENV_HOST_WAYLAND_DISPLAY, display),
            None => command.env_remove(ENV_HOST_WAYLAND_DISPLAY),
        };
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot launch `{program}` for engine `{id}`"))?;

        slot.generation += 1;
        let generation = slot.generation;
        let (kill_tx, kill_rx) = oneshot::channel();
        slot.kill = Some(kill_tx);
        slot.proxy = None;
        slot.state = EngineState::Starting {
            deadline: Instant::now() + STARTUP_TIMEOUT,
        };
        info!(
            engine = id,
            pid = child.id().unwrap_or(0),
            "engine launched"
        );

        let engine = id.to_owned();
        tokio::task::spawn_local(async move {
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
    pub fn exited(&mut self, id: &str, generation: u64, status: Option<ExitStatus>, needed: bool) {
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        if generation != slot.generation {
            return;
        }
        slot.proxy = None;
        slot.kill = None;
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

/// Accepts UI bridge connections on the engine's Wayland socket.
pub fn listen_ui(
    engine: &str,
    connections: mpsc::UnboundedSender<(String, OwnedFd)>,
) -> anyhow::Result<()> {
    let path = runtime_dir::engine_dir(engine)?.join("wayland");
    let listener = runtime_dir::bind_socket(&path)?;
    let engine = engine.to_owned();
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => match stream.into_std() {
                    Ok(stream) => {
                        if connections
                            .send((engine.clone(), OwnedFd::from(stream)))
                            .is_err()
                        {
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
