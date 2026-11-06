//! The control API on the session bus (`io.github.ign1x.OhMyIme.Daemon1`), used by the
//! `omime` command line tool and by status bars.
//!
//! Owning the bus name also makes sure only one daemon runs per session.

use omime_proto::tr;
use std::cell::RefCell;

use omime_proto::engine::Dict;
use omime_proto::names::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};
use tokio::sync::{mpsc, oneshot};
use zbus::object_server::SignalEmitter;
use zbus::{OwnedVariant, Variant};

use crate::core::{EngineSummary, InputMethod, SwitchReason};

/// Every input method and whether it is enabled, and the one in use (`None` for direct
/// input).
pub type AllInputMethods = (Vec<(InputMethod, bool)>, Option<String>);

/// Requests from D-Bus clients, answered by the daemon loop.
#[derive(Debug)]
pub enum ControlRequest {
    List(oneshot::Sender<Vec<EngineSummary>>),
    /// The input methods, and the one in use (`None` for direct input).
    ListInputMethods(oneshot::Sender<(Vec<InputMethod>, Option<String>)>),
    /// Every input method, whether it is enabled, and the one in use.
    AllInputMethods(oneshot::Sender<AllInputMethods>),
    Status(oneshot::Sender<Dict>),
    /// App id and current method before a chooser takes focus.
    ChoiceContext(oneshot::Sender<(String, String)>),
    Choose(String, String, oneshot::Sender<Result<(), String>>),
    /// `None` selects direct input.
    Switch(
        Option<String>,
        SwitchReason,
        oneshot::Sender<Result<(), String>>,
    ),
    Next(SwitchReason, oneshot::Sender<Result<(), String>>),
    Toggle(SwitchReason, oneshot::Sender<Result<(), String>>),
    Reload(oneshot::Sender<Result<(), String>>),
    /// Opens the settings program of an engine.
    OpenSettings(String, oneshot::Sender<Result<(), String>>),
    /// Opens oh-my-ime's own settings, `omime-settings`.
    OpenOwnSettings(oneshot::Sender<Result<(), String>>),
    /// The input method remembered for each application (`None`: direct input).
    Apps(oneshot::Sender<Vec<(String, Option<String>)>>),
    SetApp(String, Option<String>, oneshot::Sender<Result<(), String>>),
    ForgetApp(String, oneshot::Sender<Result<(), String>>),
    /// The configuration in effect.
    Config(oneshot::Sender<omime_proto::config::Config>),
    /// The menu of the current engine.
    Properties(oneshot::Sender<Vec<omime_proto::engine::Property>>),
    ActivateProperty(String, oneshot::Sender<Result<(), String>>),
    /// Sets options of config.toml, by dotted name, and reloads.
    Configure(
        Vec<(String, omime_proto::config::Value)>,
        oneshot::Sender<Result<(), String>>,
    ),
    Diagnose(oneshot::Sender<Dict>),
    /// What the tray item shows.
    Tray(oneshot::Sender<crate::tray::TrayInfo>),
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
            .map_err(|_| zbus::fdo::Error::Failed(tr!("the daemon is shutting down")))?;
        rx.await
            .map_err(|_| zbus::fdo::Error::Failed(tr!("the daemon is shutting down")))
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

    /// What users choose from: input methods (id, name, engine name, label, in use), in
    /// switching order. An id is an engine's (`sparkle`), or names an input method
    /// inside a hosted framework (`fcitx5/shuangpin`); `Switch` takes either.
    async fn list_input_methods(
        &self,
    ) -> zbus::fdo::Result<Vec<(String, String, String, String, bool)>> {
        let (methods, current) = self.ask(ControlRequest::ListInputMethods).await?;
        Ok(methods
            .into_iter()
            .map(|m| {
                let in_use = current.as_deref() == Some(m.id.as_str());
                (
                    m.id,
                    m.name,
                    m.engine_name,
                    m.label.unwrap_or_default(),
                    in_use,
                )
            })
            .collect())
    }

    /// Current state: "engine" (s, empty for direct input), "input-method" (s, the id
    /// `ListInputMethods` gives it), and the engine's "label", "icon" and "tooltip" (s)
    /// when it published them; for a hosted framework the tooltip names the input
    /// method in use.
    async fn status(&self) -> zbus::fdo::Result<Dict> {
        self.ask(ControlRequest::Status).await
    }

    #[zbus(out_args("app", "input_method"))]
    async fn choice_context(&self) -> zbus::fdo::Result<(String, String)> {
        self.ask(ControlRequest::ChoiceContext).await
    }

    /// Applies an explicit chooser selection to the application captured before
    /// its layer surface took focus. Empty input_method selects direct input.
    async fn choose_input_method(
        &self,
        input_method: String,
        app: String,
    ) -> zbus::fdo::Result<()> {
        failed(
            self.ask(|reply| ControlRequest::Choose(input_method, app, reply))
                .await?,
        )
    }

    /// Switches to `engine`, or to an input method inside a hosted framework
    /// (`fcitx5/shuangpin`); an empty string selects direct input.
    async fn switch(&self, engine: String) -> zbus::fdo::Result<()> {
        let target = (!engine.is_empty()).then_some(engine);
        failed(
            self.ask(|tx| ControlRequest::Switch(target, SwitchReason::Command, tx))
                .await?,
        )
    }

    /// Switches to the next enabled engine.
    async fn next(&self) -> zbus::fdo::Result<()> {
        failed(
            self.ask(|tx| ControlRequest::Next(SwitchReason::Command, tx))
                .await?,
        )
    }

    /// Switches between direct input and the last engine.
    async fn toggle(&self) -> zbus::fdo::Result<()> {
        failed(
            self.ask(|tx| ControlRequest::Toggle(SwitchReason::Command, tx))
                .await?,
        )
    }

    /// Reads the configuration again and looks for newly installed or removed engines.
    async fn reload(&self) -> zbus::fdo::Result<()> {
        failed(self.ask(ControlRequest::Reload).await?)
    }

    /// The installed engines, enabled ones first in switching order, each a dictionary:
    /// "id", "name", "description", "icon" (s), "languages" (as), "kind" (s: "engine"
    /// or "hosted", an unmodified Wayland input method), "state" (s: "stopped",
    /// "starting", "ready", "restarting" or "failed"), "enabled" and "settings" (b:
    /// whether `OpenSettings` can open its settings).
    async fn engines(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let engines = self.ask(ControlRequest::List).await?;
        Ok(engines.iter().map(engine_dict).collect())
    }

    /// Every input method there is to choose from: the enabled ones in switching order,
    /// then the ones the installed engines have besides, each a dictionary: "id" (what
    /// `Switch` and the `engines` list of config.toml take), "name", "engine",
    /// "engine-name", "icon", "label", "language" (s, empty when unknown), "enabled"
    /// and "in-use" (b).
    async fn input_methods(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let (methods, current) = self.ask(ControlRequest::AllInputMethods).await?;
        Ok(methods
            .iter()
            .map(|(method, enabled)| {
                let in_use = current.as_deref() == Some(method.id.as_str());
                input_method_dict(method, *enabled, in_use)
            })
            .collect())
    }

    /// The configuration in effect, by the dotted names of config.toml's options:
    /// "hotkeys.toggle", "hotkeys.next" (s), "engine.key_timeout_ms" (t),
    /// "initial_input_method" (s, when set; used only before any saved choice),
    /// "apps.remember", "tray.enabled", "panel.enabled", "notifications.enabled" (b),
    /// and "engines" (as) when
    /// config.toml lists engines (otherwise every engine enabled by default is).
    async fn config(&self) -> zbus::fdo::Result<Dict> {
        let config = self.ask(ControlRequest::Config).await?;
        Ok(config_dict(&config))
    }

    /// Sets options of config.toml, by the names `Config` uses ("engines" takes engines
    /// and input methods, such as `fcitx5/shuangpin`, in switching order), keeping the
    /// rest of the file, and applies them. Fails, changing nothing, when a value is not
    /// valid.
    async fn configure(&self, changes: Dict) -> zbus::fdo::Result<()> {
        let mut values = Vec::new();
        for (key, value) in changes {
            let value = config_value(&key, value).map_err(zbus::fdo::Error::InvalidArgs)?;
            values.push((key, value));
        }
        failed(self.ask(|tx| ControlRequest::Configure(values, tx)).await?)
    }

    /// The input method remembered for each application with `[apps] remember`: (app
    /// id, input method id, or empty for direct input).
    async fn apps(&self) -> zbus::fdo::Result<Vec<(String, String)>> {
        let apps = self.ask(ControlRequest::Apps).await?;
        Ok(apps
            .into_iter()
            .map(|(app, target)| (app, target.unwrap_or_default()))
            .collect())
    }

    /// Remembers `input_method` (empty for direct input) for the application `app`.
    async fn set_app_input_method(
        &self,
        app: String,
        input_method: String,
    ) -> zbus::fdo::Result<()> {
        let target = (!input_method.is_empty()).then_some(input_method);
        failed(
            self.ask(|tx| ControlRequest::SetApp(app, target, tx))
                .await?,
        )
    }

    /// Forgets the input method of the application `app`.
    async fn forget_app(&self, app: String) -> zbus::fdo::Result<()> {
        failed(self.ask(|tx| ControlRequest::ForgetApp(app, tx)).await?)
    }

    /// The menu of the input method in use: actions, switches and choices, such as
    /// Chinese or English punctuation, each a dictionary: "id", "parent" (the "menu"
    /// entry it belongs to, empty at the top), "kind" ("action", "toggle", "radio",
    /// "menu" or "separator"), "label", "icon", "tooltip" (s), "checked", "enabled" (b).
    async fn properties(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let properties = self.ask(ControlRequest::Properties).await?;
        Ok(properties
            .iter()
            .map(omime_proto::engine::Property::to_dict)
            .collect())
    }

    /// Activates a property of the input method in use.
    async fn activate_property(&self, id: String) -> zbus::fdo::Result<()> {
        failed(
            self.ask(|tx| ControlRequest::ActivateProperty(id, tx))
                .await?,
        )
    }

    /// Emitted when what `Properties` returns may have changed.
    #[zbus(signal)]
    async fn properties_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Emitted when someone switched the input method (empty: direct input), and why:
    /// "hotkey", "command" (this API), "menu" (the tray) or "engine".
    #[zbus(signal)]
    async fn switched(
        emitter: &SignalEmitter<'_>,
        input_method: &str,
        reason: &str,
    ) -> zbus::Result<()>;

    /// Emitted when the configuration in effect changed.
    #[zbus(signal)]
    async fn config_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// The version of omimed.
    #[zbus(property)]
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// Emitted when what `Engines` or `InputMethods` return may have changed: engines
    /// came or went, the configuration changed, or a framework reported its input
    /// methods.
    #[zbus(signal)]
    async fn input_methods_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Opens the settings of `engine` (its settings program, with an ordinary window).
    /// Fails if the engine has none, or it is not installed.
    async fn open_settings(&self, engine: String) -> zbus::fdo::Result<()> {
        failed(
            self.ask(|tx| ControlRequest::OpenSettings(engine, tx))
                .await?,
        )
    }

    /// What `omime doctor` checks: "display" (s, the compositor), "seats" (a(sb): every
    /// seat's name and whether omimed is its input method; another input method holds
    /// the others) and "focused-app" (b: whether the compositor reports the focused
    /// window, which per-application engines need). "input-method-configured" (b)
    /// says the engine accepted the selection; activation may still await focus.
    /// "input-method-error" (s) reports selection failure, empty when none.
    async fn diagnose(&self) -> zbus::fdo::Result<Dict> {
        self.ask(ControlRequest::Diagnose).await
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
/// What changed, for the signal of the same name with `Changed`.
enum Signal {
    Current(String),
    Status(Dict),
    InputMethods,
    Config,
    Properties,
    Switched(String, SwitchReason),
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
            let _ = self.signals.send(Signal::Current(engine));
        }
        let _ = self.signals.send(Signal::Status(status));
    }

    /// Announces that the engines or input methods to choose from may have changed.
    pub fn input_methods_changed(&self) {
        let _ = self.signals.send(Signal::InputMethods);
    }

    /// Announces that the configuration in effect changed.
    pub fn config_changed(&self) {
        let _ = self.signals.send(Signal::Config);
    }

    /// Announces that the menu of the current engine may have changed.
    pub fn properties_changed(&self) {
        let _ = self.signals.send(Signal::Properties);
    }

    /// Announces that someone switched to `input_method` (`None`: direct input).
    pub fn switched(&self, input_method: Option<&str>, reason: SwitchReason) {
        let input_method = input_method.unwrap_or_default().to_owned();
        let _ = self.signals.send(Signal::Switched(input_method, reason));
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
                Signal::Current(engine) => ControlObject::current_changed(&emitter, &engine).await,
                Signal::Status(status) => ControlObject::status_changed(&emitter, &status).await,
                Signal::InputMethods => ControlObject::input_methods_changed(&emitter).await,
                Signal::Config => ControlObject::config_changed(&emitter).await,
                Signal::Properties => ControlObject::properties_changed(&emitter).await,
                Signal::Switched(input_method, reason) => {
                    ControlObject::switched(&emitter, &input_method, reason.as_str()).await
                }
            };
        }
    });
    Ok(ControlService {
        signals,
        current: RefCell::new(None),
    })
}

fn owned(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("no file descriptors")
}

/// The reply of `Config`.
fn config_dict(config: &omime_proto::config::Config) -> Dict {
    let mut dict = Dict::from([
        (
            "hotkeys.toggle".to_owned(),
            owned(Variant::from(config.hotkeys.toggle.as_str())),
        ),
        (
            "hotkeys.next".to_owned(),
            owned(Variant::from(config.hotkeys.next.as_str())),
        ),
        (
            "engine.key_timeout_ms".to_owned(),
            owned(Variant::from(config.engine.key_timeout_ms)),
        ),
        (
            "apps.remember".to_owned(),
            owned(Variant::from(config.apps.remember)),
        ),
        (
            "apps.remember_mode".to_owned(),
            owned(Variant::from(config.apps.remember_mode)),
        ),
        (
            "tray.enabled".to_owned(),
            owned(Variant::from(config.tray.enabled)),
        ),
        (
            "panel.enabled".to_owned(),
            owned(Variant::from(config.panel.enabled)),
        ),
        (
            "notifications.enabled".to_owned(),
            owned(Variant::from(config.notifications.enabled)),
        ),
    ]);
    if let Some(engines) = &config.engines {
        dict.insert("engines".to_owned(), owned(Variant::from(engines)));
    }
    if let Some(method) = &config.initial_input_method {
        dict.insert(
            "initial_input_method".to_owned(),
            owned(Variant::from(method.as_str())),
        );
    }
    dict
}

/// A value of `Configure`, checked against the type of its option.
fn config_value(key: &str, value: OwnedVariant) -> Result<omime_proto::config::Value, String> {
    use omime_proto::config::Value;
    let wrong = || {
        tr!(
            "`{key}` does not take {arg0}",
            key = key,
            arg0 = value.value_signature()
        )
    };
    let take = || value.try_clone().map_err(|_| wrong());
    Ok(match key {
        "engines" => Value::List(Vec::<String>::try_from(take()?).map_err(|_| wrong())?),
        "hotkeys.toggle" | "hotkeys.next" | "initial_input_method" => {
            Value::String(String::try_from(take()?).map_err(|_| wrong())?)
        }
        "apps.remember"
        | "apps.remember_mode"
        | "tray.enabled"
        | "panel.enabled"
        | "notifications.enabled" => Value::Bool(bool::try_from(take()?).map_err(|_| wrong())?),
        "engine.key_timeout_ms" => Value::Integer(match &*value {
            Variant::U32(n) => i64::from(*n),
            Variant::I32(n) => i64::from(*n),
            Variant::U64(n) => i64::try_from(*n).map_err(|_| wrong())?,
            Variant::I64(n) => *n,
            _ => return Err(wrong()),
        }),
        _ => return Err(tr!("there is no option `{key}`", key = key)),
    })
}

/// An entry of the reply of `Engines`.
fn engine_dict(engine: &EngineSummary) -> Dict {
    Dict::from([
        ("id".to_owned(), owned(Variant::from(engine.id.as_str()))),
        (
            "name".to_owned(),
            owned(Variant::from(engine.name.as_str())),
        ),
        (
            "description".to_owned(),
            owned(Variant::from(
                engine.description.as_deref().unwrap_or_default(),
            )),
        ),
        (
            "icon".to_owned(),
            owned(Variant::from(engine.icon.as_deref().unwrap_or_default())),
        ),
        (
            "languages".to_owned(),
            owned(Variant::from(&engine.languages)),
        ),
        (
            "kind".to_owned(),
            owned(Variant::from(if engine.hosted {
                "hosted"
            } else {
                "engine"
            })),
        ),
        ("state".to_owned(), owned(Variant::from(engine.state))),
        ("enabled".to_owned(), owned(Variant::from(engine.enabled))),
        ("settings".to_owned(), owned(Variant::from(engine.settings))),
    ])
}

/// An entry of the reply of `InputMethods`.
fn input_method_dict(method: &InputMethod, enabled: bool, in_use: bool) -> Dict {
    let text = |value: &Option<String>| owned(Variant::from(value.as_deref().unwrap_or_default()));
    Dict::from([
        ("id".to_owned(), owned(Variant::from(method.id.as_str()))),
        (
            "name".to_owned(),
            owned(Variant::from(method.name.as_str())),
        ),
        (
            "engine".to_owned(),
            owned(Variant::from(method.engine.as_str())),
        ),
        (
            "engine-name".to_owned(),
            owned(Variant::from(method.engine_name.as_str())),
        ),
        ("icon".to_owned(), text(&method.icon)),
        ("label".to_owned(), text(&method.label)),
        ("language".to_owned(), text(&method.language)),
        ("enabled".to_owned(), owned(Variant::from(enabled))),
        ("in-use".to_owned(), owned(Variant::from(in_use))),
    ])
}

/// Builds the reply of `Diagnose`.
pub fn diagnosis(
    display: &str,
    seats: Vec<(String, bool)>,
    focused_app: bool,
    input_method_configured: bool,
    input_method_error: Option<&str>,
    input_mode_error: Option<&str>,
) -> Dict {
    let owned = |value: Variant<'_>| OwnedVariant::try_from(value).expect("no file descriptors");
    Dict::from([
        (
            "input-mode-error".to_owned(),
            owned(Variant::from(input_mode_error.unwrap_or_default())),
        ),
        ("display".to_owned(), owned(Variant::from(display))),
        ("seats".to_owned(), owned(Variant::from(seats))),
        ("focused-app".to_owned(), owned(Variant::from(focused_app))),
        (
            "input-method-configured".to_owned(),
            owned(Variant::from(input_method_configured)),
        ),
        (
            "input-method-error".to_owned(),
            owned(Variant::from(input_method_error.unwrap_or_default())),
        ),
    ])
}

/// Builds the reply of `Status`.
pub fn status_dict(
    engine: Option<&str>,
    method: Option<&str>,
    status: Option<&omime_proto::engine::Status>,
) -> Dict {
    let mut dict = status.map(|s| s.to_dict()).unwrap_or_default();
    let owned =
        |value: &str| OwnedVariant::try_from(Variant::from(value)).expect("strings can be owned");
    dict.insert("engine".into(), owned(engine.unwrap_or_default()));
    dict.insert("input-method".into(), owned(method.unwrap_or_default()));
    dict
}
