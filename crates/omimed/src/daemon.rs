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
use omime_proto::engine::{Action, ContextId, ContextInfo, KeyEvent};
use omime_proto::manifest;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::catalog::Catalog;
use crate::control::{self, ControlRequest, ControlService};
use crate::core::engines::UiConnection;
use crate::core::{Core, CoreEvent};
use crate::fcitx::{self, FcitxFrontend};
use crate::frontend::{ContextIds, Frontends, KeyDelivery, KeyRef, Origin};
use crate::placement::{self, Rect};
use crate::state::State;
use crate::tray::{self, TrayInfo, TrayService};
use crate::wayland::{EngineId, Frontend};
use crate::xim::{self, XimFrontend};

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct Options {
    /// Compositor to connect to instead of `$WAYLAND_DISPLAY`.
    pub display: Option<String>,
    /// Engine to select at startup.
    pub engine: Option<String>,
}

/// The wl-proxy poll fd, shared with the state that owns it.
pub struct PollFd(pub Rc<OwnedFd>);

impl AsRawFd for PollFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[allow(unsafe_code)]
pub fn watch(fd: PollFd) -> std::io::Result<AsyncFd<PollFd>> {
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

/// Reads config.toml; no file is the defaults. Keys this version does not know are
/// logged and left out.
fn read_config() -> Result<Config, String> {
    let Some(path) = omime_proto::paths::config_dir().map(|d| d.join(CONFIG_FILE)) else {
        return Ok(Config::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let (config, unknown) = Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    info!("using {}", path.display());
    if !unknown.is_empty() {
        warn!(
            "{}: ignoring what this version does not know: {}",
            path.display(),
            unknown.join(", ")
        );
    }
    Ok(config)
}

/// The configuration to start with: the defaults when config.toml is broken.
fn load_config() -> Config {
    read_config().unwrap_or_else(|e| {
        warn!("{e}");
        warn!("using the default configuration");
        Config::default()
    })
}

fn discover_engines() -> Vec<omime_proto::manifest::InstalledEngine> {
    let found = manifest::discover(&omime_proto::paths::engine_dirs());
    for (path, problem) in &found.broken {
        warn!("ignoring {}: {problem}", path.display());
    }
    for (engine, program) in &found.unavailable {
        debug!(engine, "not offered: `{program}` is not installed");
    }
    let installed = found.engines;
    for engine in &installed {
        if !engine.unknown_keys.is_empty() {
            warn!(
                engine = engine.id(),
                "the manifest has what this version does not know: {}",
                engine.unknown_keys.join(", ")
            );
        }
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
    /// Reloads the configuration, as daemons do.
    hup: Signal,
}

/// The tray item while it is shown, shared with the status listener.
type SharedTray = Rc<std::cell::RefCell<Option<TrayService>>>;

struct Daemon {
    options: Options,
    signals: Signals,
    /// The configuration in effect; a broken config.toml does not replace it.
    config: Config,
    core: Core,
    core_rx: mpsc::UnboundedReceiver<CoreEvent>,
    ui_rx: mpsc::UnboundedReceiver<UiConnection>,
    control_rx: mpsc::UnboundedReceiver<ControlRequest>,
    /// Kept alive for the control service's lifetime.
    control: Option<Rc<ControlService>>,
    /// For the tray item, which asks the daemon loop what to show.
    control_tx: mpsc::UnboundedSender<ControlRequest>,
    /// The tray item, unless it is turned off.
    tray: SharedTray,
    context_ids: ContextIds,
    /// Applications using the Fcitx5 module; they outlive compositor connections.
    fcitx: FcitxFrontend,
    fcitx_rx: mpsc::UnboundedReceiver<fcitx::Request>,
    /// X11 applications using XIM, while there is an X display.
    xim: Option<XimFrontend>,
    /// When to connect to the X display again, and the wait after that.
    xim_retry: Option<(Instant, Duration)>,
}

impl Daemon {
    async fn new(options: Options) -> anyhow::Result<Self> {
        let signals = Signals {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
        };

        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let control = match control::start(control_tx.clone()).await {
            Ok(service) => Some(Rc::new(service)),
            Err(e @ control::ControlError::AlreadyRunning) => return Err(e.into()),
            Err(e) => {
                warn!("{e}; `omime` and status bars will not be able to reach the daemon");
                None
            }
        };

        let context_ids = ContextIds::default();
        let (fcitx_tx, fcitx_rx) = mpsc::unbounded_channel();
        let fcitx = FcitxFrontend::new(fcitx::start(fcitx_tx).await, context_ids.clone());
        let (xim, xim_retry) = if std::env::var_os("DISPLAY").is_some() {
            match XimFrontend::connect(context_ids.clone(), xim::STARTUP_CONNECT_WAIT) {
                Ok(xim) => (Some(xim), None),
                Err(e) => {
                    warn!("XIM is not available yet: {e:#}");
                    (None, Some((Instant::now() + XIM_RETRY_MIN, XIM_RETRY_MIN)))
                }
            }
        } else {
            info!("no X display ($DISPLAY is unset); XIM is off");
            (None, None)
        };

        let config = load_config();
        let tray = SharedTray::default();
        if config.tray.enabled && control.is_some() {
            *tray.borrow_mut() = start_tray(&control_tx).await;
        }
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
        core.restore_catalog(&Catalog::load());
        core.set_catalog_listener(Catalog::save);
        let service = control.clone();
        let tray_item = tray.clone();
        core.set_status_listener(move |current, method, status| {
            if let Some(service) = &service {
                service.status_changed(current, control::status_dict(current, method, status));
            }
            if let Some(tray) = &*tray_item.borrow() {
                tray.changed();
            }
        });
        let service = control.clone();
        let tray_item = tray.clone();
        core.set_lists_listener(move || {
            if let Some(service) = &service {
                service.input_methods_changed();
            }
            if let Some(tray) = &*tray_item.borrow() {
                tray.changed();
            }
        });
        let service = control.clone();
        let tray_item = tray.clone();
        core.set_properties_listener(move || {
            if let Some(service) = &service {
                service.properties_changed();
            }
            if let Some(tray) = &*tray_item.borrow() {
                tray.changed();
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
            config,
            core,
            core_rx,
            ui_rx,
            control_rx,
            control,
            control_tx,
            tray,
            context_ids,
            fcitx,
            fcitx_rx,
            xim,
            xim_retry,
        })
    }

    async fn run(mut self) -> anyhow::Result<()> {
        let mut recent_failures: Vec<Instant> = Vec::new();
        let mut first = true;
        loop {
            // A failure to connect means the compositor is gone: that ends the daemon.
            let mut frontend =
                Frontend::connect(self.options.display.as_deref(), self.context_ids.clone())?;
            info!("connected to the compositor");
            let startup_engine = if first {
                first = false;
                self.options.engine.clone()
            } else {
                let current = self.core.current().map(Into::into);
                frontend.set_active_engine(current);
                // Engines that keep running asked for their windows on the old
                // connection.
                for engine in self.core.visible_engines() {
                    frontend.set_engine_visible(engine.into(), true);
                }
                None
            };
            let end = self.session(&mut frontend, startup_engine).await?;
            self.core.forget_wayland_contexts();
            match end {
                SessionEnd::Shutdown => {
                    self.core.shutdown();
                    // Give engines a moment to receive Shutdown, and hosted input
                    // methods, which save their state when they stop, time to exit.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let deadline = Instant::now() + ENGINE_EXIT_WAIT;
                    while self.core.engines_alive() && Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
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
                && let Err(e) = self.core.set_current(
                    Some(engine),
                    &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut()),
                )
            {
                warn!("{e:#}");
            }
            let events: Vec<_> = frontend
                .take_events()
                .into_iter()
                .chain(self.fcitx.take_events())
                .chain(self.xim.iter_mut().flat_map(XimFrontend::take_events))
                .collect();
            for event in events {
                self.core.on_frontend_event(
                    event,
                    &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut()),
                );
            }
            frontend.flush_hosted_modifiers();
            frontend.set_x11_cursor(self.x11_cursor(frontend));
            if let Err(e) = frontend.flush() {
                break SessionEnd::Lost(e);
            }
            let xim_retry = self.xim_retry.map(|(at, _)| at);
            let xim_deadline = self.xim.as_ref().and_then(XimFrontend::deadline);
            let deadline = [
                frontend.next_deadline(),
                self.core.deadline(),
                xim_retry,
                xim_deadline,
            ]
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
                    self.core.on_timer(now, &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut()));
                    if self.xim_retry.is_some_and(|(at, _)| at <= now) {
                        self.connect_xim();
                    }
                    if let Some(Err(e)) = self.xim.as_mut().map(|xim| xim.on_timer(now)) {
                        self.lose_xim(&e, frontend);
                    }
                }
                Some(event) = self.core_rx.recv() => {
                    self.core.on_event(event, &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut())).await;
                }
                Some(connection) = self.ui_rx.recv() => {
                    let engine = connection.engine.as_str().into();
                    if let Err(e) = frontend.add_ui_client(engine, connection.hosted, connection.socket) {
                        warn!("{e:#}");
                    }
                }
                Some(request) = self.control_rx.recv() => {
                    self.on_control(request, frontend).await;
                }
                Some(request) = self.fcitx_rx.recv() => self.fcitx.handle(request),
                ready = xim_readable(self.xim.as_ref()) => {
                    let result = ready.map_err(anyhow::Error::from).and_then(|()| match &mut self.xim {
                        Some(xim) => xim.dispatch(),
                        None => Ok(()),
                    });
                    if let Err(e) = result {
                        self.lose_xim(&e, frontend);
                    }
                }
                _ = self.signals.term.recv() => break SessionEnd::Shutdown,
                _ = self.signals.int.recv() => break SessionEnd::Shutdown,
                _ = self.signals.hup.recv() => {
                    info!("reloading on SIGHUP");
                    let _ = self.reload(frontend).await;
                }
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

    /// The text cursor of the focused X11 application, in compositor coordinates: of
    /// the field focused last, through either frontend.
    fn x11_cursor(&self, frontend: &Frontend) -> Option<Rect> {
        let xim = self.xim.as_ref();
        let (_, cursor) = [
            self.fcitx.focused_x11_cursor(),
            xim.and_then(XimFrontend::focused_cursor),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|(ctx, _)| *ctx)?;
        let cursor = cursor?;
        // XIM's connection describes the X screen's monitors, which map X11 coordinates.
        let x11_monitors = xim.map_or(&[][..], XimFrontend::monitors);
        Some(placement::x11_to_layout(
            cursor,
            x11_monitors,
            &frontend.monitors(),
        ))
    }

    /// Connects to the X display again after the XIM server failed.
    fn connect_xim(&mut self) {
        let Some((_, wait)) = self.xim_retry else {
            return;
        };
        match XimFrontend::connect(self.context_ids.clone(), Duration::ZERO) {
            Ok(xim) => {
                self.xim = Some(xim);
                self.xim_retry = None;
            }
            Err(e) => {
                let wait = (wait * 2).min(XIM_RETRY_MAX);
                debug!(retry_in = ?wait, "XIM is not available yet: {e:#}");
                self.xim_retry = Some((Instant::now() + wait, wait));
            }
        }
    }

    /// Ends the contexts of a failed XIM server and tries again later.
    fn lose_xim(&mut self, error: &anyhow::Error, frontend: &mut Frontend) {
        warn!("the XIM server stopped: {error:#}");
        if let Some(mut xim) = self.xim.take() {
            for event in xim.end_contexts() {
                self.core
                    .on_frontend_event(event, &mut Outputs::new(frontend, &mut self.fcitx, None));
            }
        }
        self.xim_retry = Some((Instant::now() + XIM_RETRY_MIN, XIM_RETRY_MIN));
    }

    async fn on_control(&mut self, request: ControlRequest, frontend: &mut Frontend) {
        let result = |r: anyhow::Result<()>| r.map_err(|e| format!("{e:#}"));
        match request {
            ControlRequest::List(reply) => {
                let _ = reply.send(self.core.engines());
            }
            ControlRequest::AllInputMethods(reply) => {
                let _ = reply.send((self.core.all_input_methods(), self.core.current_method()));
            }
            ControlRequest::ListInputMethods(reply) => {
                let _ = reply.send((self.core.input_methods(), self.core.current_method()));
            }
            ControlRequest::Status(reply) => {
                let _ = reply.send(control::status_dict(
                    self.core.current(),
                    self.core.current_method().as_deref(),
                    self.core.current_status().as_ref(),
                ));
            }
            ControlRequest::Switch(target, reply) => {
                let _ = reply.send(result(self.core.switch_to(
                    target,
                    &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut()),
                )));
            }
            ControlRequest::Next(reply) => {
                let _ = reply.send(result(self.core.next(&mut Outputs::new(
                    frontend,
                    &mut self.fcitx,
                    self.xim.as_mut(),
                ))));
            }
            ControlRequest::Toggle(reply) => {
                let _ = reply.send(result(self.core.toggle(&mut Outputs::new(
                    frontend,
                    &mut self.fcitx,
                    self.xim.as_mut(),
                ))));
            }
            ControlRequest::Tray(reply) => {
                let _ = reply.send(TrayInfo {
                    engine: self.core.current().map(str::to_owned),
                    engines: self.core.engines(),
                    status: self.core.current_status().unwrap_or_default(),
                    hosted: self.core.current_tray().cloned(),
                    methods: self.core.input_methods(),
                    method: self.core.current_method(),
                });
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
                let _ = reply.send(self.reload(frontend).await);
            }
            ControlRequest::Apps(reply) => {
                let _ = reply.send(self.core.apps());
            }
            ControlRequest::SetApp(app, target, reply) => {
                let outputs = &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut());
                let _ = reply.send(result(self.core.set_app(app, target, outputs)));
            }
            ControlRequest::ForgetApp(app, reply) => {
                let _ = reply.send(result(self.core.forget_app(&app)));
            }
            ControlRequest::Properties(reply) => {
                let _ = reply.send(self.core.properties());
            }
            ControlRequest::ActivateProperty(id, reply) => {
                let _ = reply.send(result(self.core.activate_property(&id)));
            }
            ControlRequest::Config(reply) => {
                let _ = reply.send(self.config.clone());
            }
            ControlRequest::Configure(changes, reply) => {
                let _ = reply.send(self.configure(changes, frontend).await);
            }
            ControlRequest::OpenSettings(engine, reply) => {
                let _ = reply.send(result(self.core.open_settings(&engine)));
            }
        }
    }

    /// Sets options of config.toml and applies them; nothing is written when a value is
    /// not valid.
    async fn configure(
        &mut self,
        changes: Vec<(String, omime_proto::config::Value)>,
        frontend: &mut Frontend,
    ) -> Result<(), String> {
        let path = omime_proto::paths::config_dir()
            .map(|dir| dir.join(CONFIG_FILE))
            .ok_or("cannot find the configuration directory")?;
        let mut text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let installed: Vec<String> = self.core.engines().into_iter().map(|e| e.id).collect();
        for (key, value) in changes {
            if let omime_proto::config::Value::List(entries) = &value {
                for entry in entries {
                    let engine = crate::core::split_target(entry).0;
                    if !installed.iter().any(|id| id == engine) {
                        return Err(format!("no engine `{engine}` is installed"));
                    }
                }
            }
            text =
                omime_proto::config::edit(&text, &key, value).map_err(|e| format!("{key}: {e}"))?;
        }
        write_atomically(&path, &text)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        self.reload(frontend).await
    }

    /// Reads the configuration again and looks for engines; a broken config.toml keeps
    /// the configuration in effect, and the engines are looked up anyway: `omime
    /// install` reloads too.
    async fn reload(&mut self, frontend: &mut Frontend) -> Result<(), String> {
        let result = read_config().map(|config| self.config = config);
        if let Err(e) = &result {
            warn!("{e}");
            warn!("keeping the configuration in effect");
        }
        let installed = discover_engines();
        self.core.reconfigure(
            &self.config,
            installed,
            &mut Outputs::new(frontend, &mut self.fcitx, self.xim.as_mut()),
        );
        let wanted = self.config.tray.enabled && self.control.is_some();
        let shown = self.tray.borrow().is_some();
        if wanted && !shown {
            let tray = start_tray(&self.control_tx).await;
            *self.tray.borrow_mut() = tray;
        } else if !wanted && shown {
            info!("the tray item is off");
            self.tray.borrow_mut().take();
        }
        // Engines may have come or gone from the menu.
        if let Some(tray) = &*self.tray.borrow() {
            tray.changed();
        }
        if result.is_ok()
            && let Some(control) = &self.control
        {
            control.config_changed();
        }
        result.map_err(|e| format!("{e}\nomimed keeps the configuration it had"))
    }
}

/// Writes `text` to `path` through a temporary file, so that a reader never sees half of
/// it.
fn write_atomically(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Shows the tray item; `None` if it cannot be shown.
async fn start_tray(requests: &mpsc::UnboundedSender<ControlRequest>) -> Option<TrayService> {
    match tray::start(requests.clone()).await {
        Ok(tray) => Some(tray),
        Err(e) => {
            warn!("cannot show the tray item: {e}");
            None
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// Waits until the XIM server has something to read; forever without one.
async fn xim_readable(xim: Option<&XimFrontend>) -> std::io::Result<()> {
    match xim {
        Some(xim) => xim.readable().await,
        None => std::future::pending().await,
    }
}

/// How long omimed waits for engines to exit when it stops.
const ENGINE_EXIT_WAIT: Duration = Duration::from_secs(2);

/// The first and the longest wait before connecting to the X display again.
const XIM_RETRY_MIN: Duration = Duration::from_secs(2);
const XIM_RETRY_MAX: Duration = Duration::from_secs(60);

/// The frontends of one compositor session, as the core sees them.
struct Outputs<'a> {
    wayland: &'a mut Frontend,
    fcitx: &'a mut FcitxFrontend,
    xim: Option<&'a mut XimFrontend>,
}

impl<'a> Outputs<'a> {
    fn new(
        wayland: &'a mut Frontend,
        fcitx: &'a mut FcitxFrontend,
        xim: Option<&'a mut XimFrontend>,
    ) -> Self {
        Self {
            wayland,
            fcitx,
            xim,
        }
    }
}

impl Frontends for Outputs<'_> {
    fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        match key {
            KeyRef::Wayland(key) => self.wayland.finish_key(key, handled),
            KeyRef::Fcitx(key) => self.fcitx.finish_key(key, handled),
            KeyRef::Xim(key) => {
                if let Some(xim) = &mut self.xim {
                    xim.finish_key(key, handled);
                }
            }
        }
    }

    fn apply_actions(&mut self, origin: Origin, ctx: ContextId, actions: &[Action]) {
        match origin {
            Origin::Wayland(seat) => self.wayland.apply_actions(seat, ctx, actions),
            Origin::Fcitx => self.fcitx.apply_actions(ctx, actions),
            Origin::Xim => {
                if let Some(xim) = &mut self.xim {
                    xim.apply_actions(ctx, actions);
                }
            }
        }
    }

    fn clear_preedit(&mut self, origin: Origin, ctx: ContextId) {
        match origin {
            Origin::Wayland(seat) => self.wayland.clear_preedit(seat, ctx),
            Origin::Fcitx => self.fcitx.clear_preedit(ctx),
            Origin::Xim => {
                if let Some(xim) = &mut self.xim {
                    xim.clear_preedit(ctx);
                }
            }
        }
    }

    fn set_active_engine(&mut self, engine: Option<EngineId>) {
        self.wayland.set_active_engine(engine);
    }

    fn set_engine_visible(&mut self, engine: EngineId, visible: bool) {
        self.wayland.set_engine_visible(engine, visible);
    }

    fn hosted_activate(
        &mut self,
        engine: &EngineId,
        origin: Origin,
        ctx: ContextId,
        info: &ContextInfo,
    ) -> bool {
        self.wayland.hosted_activate(engine, origin, ctx, info)
    }

    fn hosted_update(
        &mut self,
        engine: &EngineId,
        ctx: ContextId,
        info: &ContextInfo,
        reset: bool,
    ) {
        self.wayland.hosted_update(engine, ctx, info, reset);
    }

    fn hosted_deactivate(&mut self, engine: &EngineId, ctx: ContextId) {
        self.wayland.hosted_deactivate(engine, ctx);
    }

    fn hosted_unread(&self, engine: &EngineId) -> Option<usize> {
        self.wayland.hosted_unread(engine)
    }

    fn hosted_key(&mut self, engine: &EngineId, key: &KeyRef, event: &KeyEvent) -> KeyDelivery {
        // XIM passes only presses on, so each key is released right after its press,
        // or the input method would think it is still held. A modifier would then
        // look like a tap of its own (Fcitx5 switches modes on Shift taps), so
        // modifiers stay with the application.
        if matches!(key, KeyRef::Xim(_)) && crate::keyboard::is_modifier_key(event.keysym) {
            return KeyDelivery::Refused;
        }
        let delivery = self.wayland.hosted_key(engine, key, event);
        if delivery != KeyDelivery::Delivered {
            return delivery;
        }
        match key {
            KeyRef::Wayland(key) => self.wayland.finish_delegated(key),
            KeyRef::Fcitx(key) => self.fcitx.finish_key(key, true),
            KeyRef::Xim(xim_key) => {
                let release = KeyEvent {
                    released: true,
                    repeat: false,
                    ..event.clone()
                };
                self.wayland.hosted_key(engine, key, &release);
                if let Some(xim) = &mut self.xim {
                    xim.finish_key(xim_key, true);
                }
            }
        }
        delivery
    }
}
