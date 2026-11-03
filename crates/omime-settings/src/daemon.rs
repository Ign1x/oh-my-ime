//! omimed's control API on the session bus, on a thread of its own: reads what the window
//! shows whenever omimed says that it changed, follows omimed as it stops and starts,
//! and carries out what the user asks for.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::StreamExt as _;
use omime_proto::config::Value;
use omime_proto::names::DAEMON_BUS_NAME;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use zbus::OwnedVariant;

use crate::model::{Diagnosis, Engine, InputMethod, Options, Snapshot};

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Daemon1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait Daemon {
    fn engines(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
    fn input_methods(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
    fn config(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    fn configure(&self, changes: HashMap<&str, zbus::Variant<'_>>) -> zbus::Result<()>;
    fn apps(&self) -> zbus::Result<Vec<(String, String)>>;
    fn set_app_input_method(&self, app: &str, input_method: &str) -> zbus::Result<()>;
    fn forget_app(&self, app: &str) -> zbus::Result<()>;
    fn open_settings(&self, engine: &str) -> zbus::Result<()>;
    fn diagnose(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;

    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn input_methods_changed(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn config_changed(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn status_changed(&self, status: HashMap<String, OwnedVariant>) -> zbus::Result<()>;
}

/// What the user asks omimed for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Sets options of config.toml, by the dotted names of `Configure`.
    Configure(Vec<(String, Value)>),
    /// Opens the settings of an engine.
    OpenSettings(String),
    /// Remembers an input method (empty: direct input) for an application.
    SetAppInputMethod(String, String),
    ForgetApp(String),
}

/// What the window learns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// omimed runs, and has this.
    Snapshot(Box<Snapshot>),
    /// omimed does not run.
    Absent,
    /// What the user asked for failed: why, for people.
    Failed(String),
}

/// After a signal, how long to wait for others before reading again: switching input
/// methods or saving the configuration comes with several.
const SETTLE: Duration = Duration::from_millis(60);

/// Serves the window until it drops its end of `commands`. `deliver` takes events to the
/// window's thread.
pub fn run(commands: mpsc::UnboundedReceiver<Command>, deliver: impl Fn(Event)) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(serve(commands, &deliver)),
        Err(e) => Err(e.into()),
    };
    if let Err(e) = result {
        warn!("{e:#}");
        deliver(Event::Absent);
        deliver(Event::Failed(format!("{e:#}")));
    }
}

async fn serve(
    mut commands: mpsc::UnboundedReceiver<Command>,
    deliver: &dyn Fn(Event),
) -> anyhow::Result<()> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let daemon = DaemonProxy::builder(&conn)
        // The version is read again whenever omimed starts again.
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let bus = zbus::fdo::DBusProxy::new(&conn).await?;
    // Subscribe before reading, so that no change slips through in between.
    let mut owners = bus
        .receive_name_owner_changed_with_args(&[(0, DAEMON_BUS_NAME)])
        .await?;
    let mut input_methods = daemon.receive_input_methods_changed().await?;
    let mut config = daemon.receive_config_changed().await?;
    let mut status = daemon.receive_status_changed().await?;

    let mut last: Option<Event> = None;
    let mut due = Some(Instant::now());
    loop {
        let read_now = async {
            match due {
                Some(due) => tokio::time::sleep_until(due).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                debug!(?command, "asking omimed");
                if let Err(e) = execute(&daemon, command).await {
                    deliver(Event::Failed(message(&e)));
                }
                // What failed may have been shown as done.
                last = None;
                due = Some(Instant::now());
            }
            Some(_) = owners.next() => due = Some(Instant::now()),
            Some(_) = input_methods.next() => due = Some(settled(due)),
            Some(_) = config.next() => due = Some(settled(due)),
            Some(_) = status.next() => due = Some(settled(due)),
            () = read_now => {
                due = None;
                let event = if bus.name_has_owner(DAEMON_BUS_NAME.try_into()?).await? {
                    match read(&daemon).await {
                        Ok(snapshot) => Event::Snapshot(Box::new(snapshot)),
                        Err(e) => {
                            warn!("cannot read the settings: {e}");
                            // It may have just stopped, which a signal will say.
                            continue;
                        }
                    }
                } else {
                    Event::Absent
                };
                if last.as_ref() != Some(&event) {
                    match &event {
                        Event::Snapshot(snapshot) => info!(
                            input_methods = snapshot.input_methods.len(),
                            "read the settings"
                        ),
                        Event::Absent => info!("omimed is not running"),
                        Event::Failed(_) => {}
                    }
                    deliver(event.clone());
                    last = Some(event);
                }
            }
        }
    }
}

/// When to read after a signal: soon, unless reading is already due earlier.
fn settled(due: Option<Instant>) -> Instant {
    let soon = Instant::now() + SETTLE;
    due.map_or(soon, |due| due.min(soon))
}

/// The message of a failed call, without D-Bus's error name.
fn message(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(_, Some(message), _) => message.clone(),
        zbus::Error::FDO(e) => match &**e {
            zbus::fdo::Error::Failed(message) | zbus::fdo::Error::InvalidArgs(message) => {
                message.clone()
            }
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}

async fn execute(daemon: &DaemonProxy<'_>, command: Command) -> zbus::Result<()> {
    match command {
        Command::Configure(changes) => {
            let changes: HashMap<&str, zbus::Variant<'_>> = changes
                .iter()
                .map(|(key, value)| (key.as_str(), variant(value)))
                .collect();
            daemon.configure(changes).await
        }
        Command::OpenSettings(engine) => daemon.open_settings(&engine).await,
        Command::SetAppInputMethod(app, method) => daemon.set_app_input_method(&app, &method).await,
        Command::ForgetApp(app) => daemon.forget_app(&app).await,
    }
}

fn variant(value: &Value) -> zbus::Variant<'_> {
    match value {
        Value::Bool(b) => zbus::Variant::from(*b),
        Value::Integer(n) => zbus::Variant::from(u64::try_from(*n).unwrap_or_default()),
        Value::String(s) => zbus::Variant::from(s.as_str()),
        Value::List(items) => zbus::Variant::from(items.clone()),
    }
}

/// Reads everything the window shows.
async fn read(daemon: &DaemonProxy<'_>) -> zbus::Result<Snapshot> {
    let (version, methods, engines, config, apps, diagnosis) = tokio::try_join!(
        daemon.version(),
        daemon.input_methods(),
        daemon.engines(),
        daemon.config(),
        daemon.apps(),
        daemon.diagnose(),
    )?;
    Ok(Snapshot {
        version,
        input_methods: methods.iter().map(input_method).collect(),
        engines: engines.iter().map(engine).collect(),
        options: options(&config),
        apps,
        diagnosis: diagnose(&diagnosis),
    })
}

type Dict = HashMap<String, OwnedVariant>;

fn text(dict: &Dict, key: &str) -> String {
    dict.get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

fn flag(dict: &Dict, key: &str) -> Option<bool> {
    dict.get(key)
        .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
}

fn input_method(dict: &Dict) -> InputMethod {
    InputMethod {
        id: text(dict, "id"),
        name: text(dict, "name"),
        engine: text(dict, "engine"),
        engine_name: text(dict, "engine-name"),
        label: text(dict, "label"),
        language: text(dict, "language"),
        enabled: flag(dict, "enabled").unwrap_or_default(),
        in_use: flag(dict, "in-use").unwrap_or_default(),
    }
}

fn engine(dict: &Dict) -> Engine {
    Engine {
        id: text(dict, "id"),
        name: text(dict, "name"),
        settings: flag(dict, "settings").unwrap_or_default(),
    }
}

fn options(dict: &Dict) -> Options {
    let defaults = Options::default();
    let timeout = dict
        .get("engine.key_timeout_ms")
        .and_then(|v| u64::try_from(v.try_clone().ok()?).ok());
    // An empty hotkey is one that is off.
    let hotkey = |key: &str, default: String| {
        if dict.contains_key(key) {
            text(dict, key)
        } else {
            default
        }
    };
    Options {
        toggle: hotkey("hotkeys.toggle", defaults.toggle),
        next: hotkey("hotkeys.next", defaults.next),
        key_timeout_ms: timeout.unwrap_or(defaults.key_timeout_ms),
        remember_apps: flag(dict, "apps.remember").unwrap_or(defaults.remember_apps),
        tray: flag(dict, "tray.enabled").unwrap_or(defaults.tray),
        panel: flag(dict, "panel.enabled").unwrap_or(defaults.panel),
    }
}

fn diagnose(dict: &Dict) -> Diagnosis {
    Diagnosis {
        display: text(dict, "display"),
        seats: dict
            .get("seats")
            .and_then(|v| Vec::<(String, bool)>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default(),
        focused_app: flag(dict, "focused-app").unwrap_or_default(),
        error: text(dict, "input-method-error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(value: zbus::Variant<'_>) -> OwnedVariant {
        OwnedVariant::try_from(value).expect("no file descriptors")
    }

    #[test]
    fn options_missing_from_the_reply_keep_their_defaults() {
        let dict = Dict::from([
            ("hotkeys.toggle".to_owned(), owned("".into())),
            ("engine.key_timeout_ms".to_owned(), owned(500u64.into())),
            ("apps.remember".to_owned(), owned(true.into())),
        ]);
        let options = options(&dict);
        assert_eq!(options.toggle, "", "a hotkey that is off");
        assert_eq!(options.next, Options::default().next);
        assert_eq!(options.key_timeout_ms, 500);
        assert!(options.remember_apps);
        assert!(options.tray);
    }

    #[test]
    fn the_diagnosis_has_the_seats() {
        let seats = vec![("seat0".to_owned(), true), ("seat1".to_owned(), false)];
        let dict = Dict::from([
            ("display".to_owned(), owned("wayland-1".into())),
            ("seats".to_owned(), owned(seats.clone().into())),
            ("focused-app".to_owned(), owned(true.into())),
        ]);
        let diagnosis = diagnose(&dict);
        assert_eq!(diagnosis.seats, seats);
        assert_eq!(diagnosis.display, "wayland-1");
        assert!(diagnosis.focused_app);
        assert_eq!(diagnosis.error, "");
    }

    #[test]
    fn failures_read_as_their_message() {
        let error = zbus::Error::FDO(Box::new(zbus::fdo::Error::Failed(
            "`fcitx5` has no settings".to_owned(),
        )));
        assert_eq!(message(&error), "`fcitx5` has no settings");
    }
}
