//! The control API on the session bus (`io.github.ign1x.OhMyIme.Daemon1`), used by the
//! `omime` command line tool and by status bars.
//!
//! Owning the bus name also makes sure only one daemon runs per session.

use std::cell::RefCell;

use omime_proto::engine::Dict;
use omime_proto::names::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};
use tokio::sync::{mpsc, oneshot};
use zbus::object_server::SignalEmitter;
use zbus::{OwnedVariant, Variant};

use crate::core::EngineSummary;

/// Requests from D-Bus clients, answered by the daemon loop.
#[derive(Debug)]
pub enum ControlRequest {
    List(oneshot::Sender<Vec<EngineSummary>>),
    Status(oneshot::Sender<Dict>),
    /// `None` selects direct input.
    Switch(Option<String>, oneshot::Sender<Result<(), String>>),
    Next(oneshot::Sender<Result<(), String>>),
    Toggle(oneshot::Sender<Result<(), String>>),
    Reload(oneshot::Sender<Result<(), String>>),
}

struct ControlObject {
    requests: mpsc::UnboundedSender<ControlRequest>,
}

impl ControlObject {
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> ControlRequest,
    ) -> zbus::fdo::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(make(tx))
            .map_err(|_| zbus::fdo::Error::Failed("the daemon is shutting down".into()))?;
        rx.await
            .map_err(|_| zbus::fdo::Error::Failed("the daemon is shutting down".into()))
    }
}

fn failed(result: Result<(), String>) -> zbus::fdo::Result<()> {
    result.map_err(zbus::fdo::Error::Failed)
}

#[zbus::interface(name = "io.github.ign1x.OhMyIme.Daemon1")]
impl ControlObject {
    /// Installed engines: (id, name, enabled, state). Enabled engines come first, in
    /// switching order.
    async fn list_engines(&self) -> zbus::fdo::Result<Vec<(String, String, bool, String)>> {
        let engines = self.ask(ControlRequest::List).await?;
        Ok(engines
            .into_iter()
            .map(|e| (e.id, e.name, e.enabled, e.state.to_owned()))
            .collect())
    }

    /// Current state: "engine" (s, empty for direct input), and the engine's "label",
    /// "icon" and "tooltip" (s) when it published them.
    async fn status(&self) -> zbus::fdo::Result<Dict> {
        self.ask(ControlRequest::Status).await
    }

    /// Switches to `engine`; an empty string selects direct input.
    async fn switch(&self, engine: String) -> zbus::fdo::Result<()> {
        let target = (!engine.is_empty()).then_some(engine);
        failed(self.ask(|tx| ControlRequest::Switch(target, tx)).await?)
    }

    /// Switches to the next enabled engine.
    async fn next(&self) -> zbus::fdo::Result<()> {
        failed(self.ask(ControlRequest::Next).await?)
    }

    /// Switches between direct input and the last engine.
    async fn toggle(&self) -> zbus::fdo::Result<()> {
        failed(self.ask(ControlRequest::Toggle).await?)
    }

    /// Reads the configuration again and looks for newly installed or removed engines.
    async fn reload(&self) -> zbus::fdo::Result<()> {
        failed(self.ask(ControlRequest::Reload).await?)
    }

    /// Emitted when the current engine changes (empty for direct input).
    #[zbus(signal)]
    async fn current_changed(emitter: &SignalEmitter<'_>, engine: &str) -> zbus::Result<()>;

    /// Emitted when what `Status` returns changes, because the current engine changed or
    /// published a new status. Carries the new status.
    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>, status: &Dict) -> zbus::Result<()>;
}

/// A signal waiting to be emitted.
#[derive(Debug)]
enum Signal {
    CurrentChanged(String),
    StatusChanged(Dict),
}

/// The running control service.
#[derive(Debug)]
pub struct ControlService {
    signals: mpsc::UnboundedSender<Signal>,
    /// The engine last announced with `CurrentChanged`.
    current: RefCell<Option<String>>,
}

impl ControlService {
    /// Announces the status of the current engine `engine` (see [`status_dict`]), and the
    /// engine itself if it changed.
    pub fn status_changed(&self, engine: Option<&str>, status: Dict) {
        if self.current.borrow().as_deref() != engine {
            self.current.replace(engine.map(str::to_owned));
            let engine = engine.unwrap_or_default().to_owned();
            let _ = self.signals.send(Signal::CurrentChanged(engine));
        }
        let _ = self.signals.send(Signal::StatusChanged(status));
    }
}

/// Error starting the service.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("another oh-my-ime daemon already runs in this session")]
    AlreadyRunning,
    #[error("the session bus is not available: {0}")]
    NoBus(zbus::Error),
}

/// Connects to the session bus, takes the daemon's name and serves the control API.
pub async fn start(
    requests: mpsc::UnboundedSender<ControlRequest>,
) -> Result<ControlService, ControlError> {
    let conn = zbus::connection::Builder::session()
        .serve_at(DAEMON_OBJECT_PATH, ControlObject { requests })
        .build()
        .await
        .map_err(ControlError::NoBus)?;
    let flags = zbus::fdo::RequestNameFlags::DO_NOT_QUEUE;
    match conn.request_name_with_flags(DAEMON_BUS_NAME, flags).await {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => return Err(ControlError::AlreadyRunning),
        Err(e) => return Err(ControlError::NoBus(e)),
    }

    let (signals, mut pending) = mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        let Ok(emitter) = SignalEmitter::new(&conn, DAEMON_OBJECT_PATH) else {
            return;
        };
        while let Some(signal) = pending.recv().await {
            let _ = match signal {
                Signal::CurrentChanged(engine) => {
                    ControlObject::current_changed(&emitter, &engine).await
                }
                Signal::StatusChanged(status) => {
                    ControlObject::status_changed(&emitter, &status).await
                }
            };
        }
    });
    Ok(ControlService {
        signals,
        current: RefCell::new(None),
    })
}

/// Builds the reply of `Status`.
pub fn status_dict(engine: Option<&str>, status: Option<&omime_proto::engine::Status>) -> Dict {
    let mut dict = status.map(|s| s.to_dict()).unwrap_or_default();
    dict.insert(
        "engine".into(),
        OwnedVariant::try_from(Variant::from(engine.unwrap_or_default()))
            .expect("strings can be owned"),
    );
    dict
}
