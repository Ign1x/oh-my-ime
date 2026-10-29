//! The control API on the session bus (`io.github.ign1x.OhMyIme.Daemon1`), used by the
//! `omime` command line tool and by status bars.
//!
//! Owning the bus name also makes sure only one daemon runs per session.

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
}

/// The running control service.
#[derive(Debug)]
pub struct ControlService {
    signals: mpsc::UnboundedSender<String>,
}

impl ControlService {
    /// Announces a new current engine.
    pub fn current_changed(&self, engine: Option<&str>) {
        let _ = self.signals.send(engine.unwrap_or_default().to_owned());
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

    let (signals, mut pending) = mpsc::unbounded_channel::<String>();
    tokio::task::spawn_local(async move {
        let Ok(emitter) = SignalEmitter::new(&conn, DAEMON_OBJECT_PATH) else {
            return;
        };
        while let Some(engine) = pending.recv().await {
            let _ = ControlObject::current_changed(&emitter, &engine).await;
        }
    });
    Ok(ControlService { signals })
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
