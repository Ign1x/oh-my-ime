//! The daemon's event loop.
//!
//! Everything runs on one thread: the wl-proxy state is not `Send`, and keeping a
//! single owner for all input state makes ordering trivial to reason about. Background
//! tasks (engine calls, sockets, D-Bus) report back through channels.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_proto::config::{CONFIG_FILE, Config};
use omime_proto::engine::ContextId;
use omime_proto::manifest;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::control::{self, ControlRequest, ControlService};
use crate::core::{Core, CoreEvent};
use crate::state::State;
use crate::wayland::Frontend;

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct Options {
    /// Compositor to connect to instead of `$WAYLAND_DISPLAY`.
    pub display: Option<String>,
    /// Engine to select at startup.
    pub engine: Option<String>,
}

/// The wl-proxy poll fd, shared with the state that owns it.
struct PollFd(Rc<OwnedFd>);

impl AsRawFd for PollFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[allow(unsafe_code)]
fn watch(fd: PollFd) -> std::io::Result<AsyncFd<PollFd>> {
    // SAFETY: `PollFd` keeps its own reference to the `OwnedFd`, so the descriptor stays
    // open for as long as the `AsyncFd` exists, and `as_raw_fd` always returns it.
    unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }.map_err(Into::into)
}

/// Runs the daemon until it is asked to stop.
pub fn run(options: Options) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async { Daemon::new(options).await?.run().await })
}

fn load_config() -> Config {
    let Some(path) = omime_proto::paths::config_dir().map(|d| d.join(CONFIG_FILE)) else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => {
                info!("using {}", path.display());
                config
            }
            Err(e) => {
                warn!("ignoring {}: {e}", path.display());
                Config::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            warn!("cannot read {}: {e}", path.display());
            Config::default()
        }
    }
}

fn discover_engines() -> Vec<omime_proto::manifest::InstalledEngine> {
    let (installed, problems) = manifest::discover(&omime_proto::paths::engine_dirs());
    for (path, problem) in problems {
        warn!("ignoring {}: {problem}", path.display());
    }
    installed
}

/// Why a compositor session ended.
enum SessionEnd {
    /// We were asked to stop.
    Shutdown,
    /// The connection broke; it may be re-established.
    Lost(anyhow::Error),
}

struct Signals {
    term: Signal,
    int: Signal,
}

struct Daemon {
    options: Options,
    signals: Signals,
    core: Core,
    core_rx: mpsc::UnboundedReceiver<CoreEvent>,
    ui_rx: mpsc::UnboundedReceiver<(String, OwnedFd)>,
    control_rx: mpsc::UnboundedReceiver<ControlRequest>,
    /// Kept alive for the control service's lifetime.
    _control: Option<Rc<ControlService>>,
    last_ctx: ContextId,
}

impl Daemon {
    async fn new(options: Options) -> anyhow::Result<Self> {
        let signals = Signals {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
        };

        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let control = match control::start(control_tx).await {
            Ok(service) => Some(Rc::new(service)),
            Err(e @ control::ControlError::AlreadyRunning) => return Err(e.into()),
            Err(e) => {
                warn!("{e}; `omime` and status bars will not be able to reach the daemon");
                None
            }
        };

        let config = load_config();
        let installed = discover_engines();
        if installed.is_empty() {
            warn!("no engines are installed; keys pass through unchanged");
        } else {
            let ids: Vec<&str> = installed.iter().map(|e| e.id()).collect();
            info!("installed engines: {}", ids.join(", "));
        }

        let (core_tx, core_rx) = mpsc::unbounded_channel();
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        let host_display = options
            .display
            .clone()
            .or_else(|| std::env::var("WAYLAND_DISPLAY").ok());
        let mut core = Core::new(&config, installed, core_tx, ui_tx, host_display);
        let saved = State::load();
        core.restore(&saved);
        core.set_state_listener(State::save);
        let service = control.clone();
        core.set_status_listener(move |current, status| {
            if let Some(service) = &service {
                service.status_changed(current, control::status_dict(current, status));
            }
        });
        // Without --engine, start where the user left off.
        let mut options = options;
        if options.engine.is_none() {
            options.engine = saved.current.filter(|e| core.is_enabled(e));
        }
        Ok(Self {
            options,
            signals,
            core,
            core_rx,
            ui_rx,
            control_rx,
            _control: control,
            last_ctx: 0,
        })
    }

    async fn run(mut self) -> anyhow::Result<()> {
        let mut recent_failures: Vec<Instant> = Vec::new();
        let mut first = true;
        loop {
            // A failure to connect means the compositor is gone: that ends the daemon.
            let mut frontend = Frontend::connect(self.options.display.as_deref(), self.last_ctx)?;
            info!("connected to the compositor");
            let startup_engine = if first {
                first = false;
                self.options.engine.clone()
            } else {
                let current = self.core.current().map(Into::into);
                frontend.set_active_engine(current);
                None
            };
            let end = self.session(&mut frontend, startup_engine).await?;
            self.last_ctx = frontend.last_context_id();
            self.core.forget_contexts();
            match end {
                SessionEnd::Shutdown => {
                    self.core.shutdown();
                    // Give engines a moment to receive Shutdown.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(());
                }
                SessionEnd::Lost(e) => {
                    error!("lost the compositor connection: {e:#}");
                    let now = Instant::now();
                    recent_failures.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                    recent_failures.push(now);
                    if recent_failures.len() > 5 {
                        self.core.shutdown();
                        return Err(e.context("the connection keeps failing; giving up"));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    info!("reconnecting");
                }
            }
        }
    }

    /// Runs until the compositor connection ends or we are asked to stop.
    async fn session(
        &mut self,
        frontend: &mut Frontend,
        mut startup_engine: Option<String>,
    ) -> anyhow::Result<SessionEnd> {
        let poll_fd = watch(PollFd(frontend.state().poll_fd().clone()))
            .context("failed to watch the Wayland connection")?;

        let end = loop {
            // Launch the startup engine once the compositor's globals are known, so
            // that its UI connection finds a complete registry.
            if frontend.is_ready()
                && let Some(engine) = startup_engine.take()
                && let Err(e) = self.core.set_current(Some(engine), frontend)
            {
                warn!("{e:#}");
            }
            for event in frontend.take_events() {
                self.core.on_frontend_event(event, frontend);
            }
            if let Err(e) = frontend.flush() {
                break SessionEnd::Lost(e);
            }
            let deadline = [frontend.next_deadline(), self.core.deadline()]
                .into_iter()
                .flatten()
                .min();
            tokio::select! {
                ready = poll_fd.readable() => {
                    let mut guard = match ready {
                        Ok(guard) => guard,
                        Err(e) => break SessionEnd::Lost(e.into()),
                    };
                    guard.clear_ready();
                    if let Err(e) = frontend.dispatch() {
                        break SessionEnd::Lost(e);
                    }
                }
                () = sleep_until(deadline) => {
                    let now = Instant::now();
                    if let Err(e) = frontend.on_timer(now) {
                        break SessionEnd::Lost(e);
                    }
                    self.core.on_timer(now, frontend);
                }
                Some(event) = self.core_rx.recv() => {
                    self.core.on_event(event, frontend).await;
                }
                Some((engine, fd)) = self.ui_rx.recv() => {
                    if let Err(e) = frontend.add_ui_client(engine.as_str().into(), fd) {
                        warn!("{e:#}");
                    }
                }
                Some(request) = self.control_rx.recv() => {
                    self.on_control(request, frontend);
                }
                _ = self.signals.term.recv() => break SessionEnd::Shutdown,
                _ = self.signals.int.recv() => break SessionEnd::Shutdown,
            }
        };

        if matches!(end, SessionEnd::Shutdown) {
            info!("shutting down");
        }
        frontend.shutdown();
        // Best effort: the connection may already be gone.
        let _ = frontend.flush();
        frontend.state().destroy();
        Ok(end)
    }

    fn on_control(&mut self, request: ControlRequest, frontend: &mut Frontend) {
        let result = |r: anyhow::Result<()>| r.map_err(|e| format!("{e:#}"));
        match request {
            ControlRequest::List(reply) => {
                let _ = reply.send(self.core.engines());
            }
            ControlRequest::Status(reply) => {
                let _ = reply.send(control::status_dict(
                    self.core.current(),
                    self.core.current_status(),
                ));
            }
            ControlRequest::Switch(target, reply) => {
                let _ = reply.send(result(self.core.switch_to(target, frontend)));
            }
            ControlRequest::Next(reply) => {
                let _ = reply.send(result(self.core.next(frontend)));
            }
            ControlRequest::Toggle(reply) => {
                let _ = reply.send(result(self.core.toggle(frontend)));
            }
            ControlRequest::Diagnose(reply) => {
                let display = self
                    .options
                    .display
                    .clone()
                    .or_else(|| std::env::var("WAYLAND_DISPLAY").ok())
                    .unwrap_or_default();
                let _ = reply.send(control::diagnosis(
                    &display,
                    frontend.seat_states(),
                    frontend.reports_focused_app(),
                ));
            }
            ControlRequest::Reload(reply) => {
                let config = load_config();
                let installed = discover_engines();
                self.core.reconfigure(&config, installed, frontend);
                let _ = reply.send(Ok(()));
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}
