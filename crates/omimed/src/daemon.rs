//! The daemon's event loop.
//!
//! Everything runs on one thread: the wl-proxy state is not `Send`, and keeping a
//! single owner for all input state makes ordering trivial to reason about.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::runtime_dir;
use crate::wayland::{EngineId, Frontend};

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct Options {
    /// Compositor to connect to instead of `$WAYLAND_DISPLAY`.
    pub display: Option<String>,
    /// Engine ids for which to open UI bridge sockets without launching an engine.
    /// Used to try toolkits against the bridge during development.
    pub dev_ui: Vec<String>,
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
    local.block_on(&runtime, async { Daemon::new(options)?.run().await })
}

/// A connection to an engine's UI socket.
type UiConnection = (EngineId, OwnedFd);

/// Listens on an engine's UI socket and passes connections to the main loop.
fn listen_ui(engine: &str, connections: mpsc::UnboundedSender<UiConnection>) -> anyhow::Result<()> {
    let path = runtime_dir::engine_dir(engine)?.join("wayland");
    let listener = runtime_dir::bind_socket(&path)?;
    info!(
        engine,
        "UI bridge socket ready: WAYLAND_DISPLAY={}",
        runtime_dir::engine_wayland_display(engine)
    );
    let engine: EngineId = engine.into();
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let fd = match stream.into_std() {
                        Ok(stream) => OwnedFd::from(stream),
                        Err(e) => {
                            warn!("cannot take over a UI connection: {e}");
                            continue;
                        }
                    };
                    if connections.send((engine.clone(), fd)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!(engine = &*engine, "accepting a UI connection failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok(())
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
    usr1: Signal,
}

struct Daemon {
    options: Options,
    signals: Signals,
    ui_rx: mpsc::UnboundedReceiver<UiConnection>,
    dev_engines: Vec<EngineId>,
    dev_active: usize,
}

impl Daemon {
    fn new(options: Options) -> anyhow::Result<Self> {
        let signals = Signals {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            usr1: signal(SignalKind::user_defined1())?,
        };
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        // Sockets outlive compositor connections; engines simply reconnect.
        for engine in &options.dev_ui {
            listen_ui(engine, ui_tx.clone())?;
        }
        let dev_engines = options.dev_ui.iter().map(|e| e.as_str().into()).collect();
        Ok(Self {
            options,
            signals,
            ui_rx,
            dev_engines,
            dev_active: 0,
        })
    }

    async fn run(mut self) -> anyhow::Result<()> {
        let mut recent_failures: Vec<Instant> = Vec::new();
        loop {
            // A failure to connect means the compositor is gone: that ends the daemon.
            let frontend = Frontend::connect(self.options.display.as_deref())?;
            info!("connected to the compositor");
            match self.session(frontend).await? {
                SessionEnd::Shutdown => return Ok(()),
                SessionEnd::Lost(e) => {
                    error!("lost the compositor connection: {e:#}");
                    let now = Instant::now();
                    recent_failures.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                    recent_failures.push(now);
                    if recent_failures.len() > 5 {
                        return Err(e.context("the connection keeps failing; giving up"));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    info!("reconnecting");
                }
            }
        }
    }

    /// Runs until the compositor connection ends or we are asked to stop.
    async fn session(&mut self, mut frontend: Frontend) -> anyhow::Result<SessionEnd> {
        let poll_fd = watch(PollFd(frontend.state().poll_fd().clone()))
            .context("failed to watch the Wayland connection")?;
        frontend.set_active_engine(self.dev_engines.get(self.dev_active).cloned());

        let end = loop {
            if let Err(e) = frontend.flush() {
                break SessionEnd::Lost(e);
            }
            let deadline = frontend.next_deadline();
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
                    if let Err(e) = frontend.on_timer(Instant::now()) {
                        break SessionEnd::Lost(e);
                    }
                }
                Some((engine, fd)) = self.ui_rx.recv() => {
                    if let Err(e) = frontend.add_ui_client(engine, fd) {
                        warn!("{e:#}");
                    }
                }
                _ = self.signals.usr1.recv() => {
                    // Development aid: cycle through the --dev-ui engines.
                    if !self.dev_engines.is_empty() {
                        self.dev_active = (self.dev_active + 1) % self.dev_engines.len();
                        let engine = self.dev_engines[self.dev_active].clone();
                        info!(engine = &*engine, "active engine");
                        frontend.set_active_engine(Some(engine));
                    }
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
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}
