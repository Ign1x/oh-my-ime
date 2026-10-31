//! The input methods inside hosted frameworks.
//!
//! A hosted Fcitx 5 or IBus is one engine for omimed, with many input methods inside:
//! Fcitx 5's 双拼 and 拼音, IBus's libpinyin. Users choose input methods, not
//! frameworks, so omimed asks the framework which input methods it has, which one is in
//! use, and switches between them: Fcitx 5 through its controller on the private bus,
//! IBus through its own bus, whose address `omime-ibus` leaves in the engine's runtime
//! directory.
//!
//! One adapter task runs for each launch of a hosted engine. It reports what it learns
//! as [`CoreEvent::Framework`] and takes [`FrameworkCommand`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};
use zbus::{OwnedVariant, Structure};

use super::CoreEvent;

/// How long a framework may take to appear on the bus after it was launched.
const DETECT_TIMEOUT: Duration = Duration::from_secs(30);
const DETECT_INTERVAL: Duration = Duration::from_millis(250);

const FCITX_NAME: &str = "org.fcitx.Fcitx5";
const FCITX_PATH: &str = "/controller";
const FCITX_INTERFACE: &str = "org.fcitx.Fcitx.Controller1";
const IBUS_NAME: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IBUS_INTERFACE: &str = "org.freedesktop.IBus";
/// The dconf key of the engines IBus users chose.
const IBUS_PRELOAD_KEY: &str = "/desktop/ibus/general/preload-engines";

/// The file in a hosted engine's runtime directory naming IBus's address file.
pub const IBUS_ADDRESS_FILE: &str = "ibus-address-file";

/// An input method inside a framework.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InnerMethod {
    /// The framework's name for it: `shuangpin`, `libpinyin`.
    pub id: String,
    pub name: String,
    /// An icon name or file.
    pub icon: String,
    /// A short label, such as 双.
    pub label: String,
}

/// What the core asks of an adapter.
#[derive(Debug)]
pub enum FrameworkCommand {
    /// Make this input method the framework's.
    Select(String),
    /// Something may have changed (the framework's tray item did): look again.
    Refresh,
}

/// What an adapter reports.
#[derive(Debug)]
pub enum FrameworkUpdate {
    /// The adapter found the framework and takes commands.
    Connected(mpsc::UnboundedSender<FrameworkCommand>),
    /// The input methods the user set up in the framework.
    Methods(Vec<InnerMethod>),
    /// The input method in use; `None` when the framework passes keys on (Fcitx 5's
    /// keyboard layouts, IBus's `xkb:` engines).
    Current(Option<String>),
}

/// The adapter of one launch. Dropping it stops the adapter.
#[derive(Debug)]
pub struct FrameworkAdapter(tokio::task::JoinHandle<()>);

impl Drop for FrameworkAdapter {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts the adapter of the hosted engine `engine` (launch `generation`), whose
/// private bus is at `address` and runtime directory at `dir`.
pub fn start(
    engine: String,
    generation: u64,
    address: String,
    dir: PathBuf,
    events: mpsc::UnboundedSender<CoreEvent>,
) -> FrameworkAdapter {
    let task = tokio::task::spawn_local(async move {
        let report = |update| {
            let _ = events.send(CoreEvent::Framework {
                engine: engine.clone(),
                generation,
                update,
            });
        };
        let result = async {
            let Some(framework) = detect(&address, &dir).await? else {
                debug!(engine, "no framework with input methods of its own");
                return Ok(());
            };
            info!(
                engine,
                framework = framework.name(),
                "found the framework's input methods"
            );
            framework.serve(&report).await
        }
        .await;
        if let Err(e) = result {
            debug!(
                engine,
                "stopped following the framework's input methods: {e}"
            );
        }
    });
    FrameworkAdapter(task)
}

enum Framework {
    Fcitx5(zbus::Proxy<'static>),
    Ibus(zbus::Proxy<'static>),
}

/// Waits until Fcitx 5 owns its name on the private bus, or `omime-ibus` tells where
/// IBus is.
async fn detect(address: &str, dir: &Path) -> zbus::Result<Option<Framework>> {
    let private = zbus::connection::Builder::address(address).build().await?;
    let dbus = zbus::fdo::DBusProxy::new(&private).await?;
    let deadline = tokio::time::Instant::now() + DETECT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if dbus.name_has_owner(FCITX_NAME.try_into()?).await? {
            let controller = proxy(&private, FCITX_NAME, FCITX_PATH, FCITX_INTERFACE).await?;
            return Ok(Some(Framework::Fcitx5(controller)));
        }
        if let Some(address) = ibus_address(dir)
            && let Ok(conn) = zbus::connection::Builder::address(address.as_str())
                .build()
                .await
        {
            let ibus = proxy(&conn, IBUS_NAME, IBUS_PATH, IBUS_INTERFACE).await?;
            return Ok(Some(Framework::Ibus(ibus)));
        }
        tokio::time::sleep(DETECT_INTERVAL).await;
    }
    Ok(None)
}

async fn proxy(
    conn: &zbus::Connection,
    name: &'static str,
    path: &'static str,
    interface: &'static str,
) -> zbus::Result<zbus::Proxy<'static>> {
    zbus::proxy::Builder::new(conn)
        .destination(name)
        .path(path)
        .interface(interface)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// IBus's address: `omime-ibus` names the file where IBus wrote it.
fn ibus_address(dir: &Path) -> Option<String> {
    let file = std::fs::read_to_string(dir.join(IBUS_ADDRESS_FILE)).ok()?;
    let text = std::fs::read_to_string(file.trim()).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("IBUS_ADDRESS="))
        .map(str::to_owned)
}

impl Framework {
    fn name(&self) -> &'static str {
        match self {
            Self::Fcitx5(_) => "Fcitx 5",
            Self::Ibus(_) => "IBus",
        }
    }

    async fn serve(self, report: &impl Fn(FrameworkUpdate)) -> zbus::Result<()> {
        let (commands, mut pending) = mpsc::unbounded_channel();
        report(FrameworkUpdate::Connected(commands));
        let mut methods = self.methods(&[]).await?;
        report(FrameworkUpdate::Methods(methods.clone()));
        report(FrameworkUpdate::Current(self.current().await?));
        // What tells that the list or the current input method changed.
        let mut changes = match &self {
            Self::Fcitx5(controller) => {
                controller.receive_signal("InputMethodGroupChanged").await?
            }
            Self::Ibus(ibus) => ibus.receive_signal("GlobalEngineChanged").await?,
        };
        let mut gone = self.proxy().receive_owner_changed().await?;
        loop {
            let refresh_methods = tokio::select! {
                change = changes.next() => {
                    if change.is_none() {
                        break;
                    }
                    true
                }
                command = pending.recv() => match command {
                    None => break,
                    Some(FrameworkCommand::Select(id)) => {
                        self.select(&id).await?;
                        false
                    }
                    Some(FrameworkCommand::Refresh) => false,
                },
                owner = gone.next() => {
                    if !matches!(owner, Some(Some(_))) {
                        break;
                    }
                    false
                }
            };
            if refresh_methods {
                let now = self.methods(&methods).await?;
                if now != methods {
                    methods = now;
                    report(FrameworkUpdate::Methods(methods.clone()));
                }
            }
            report(FrameworkUpdate::Current(self.current().await?));
        }
        Ok(())
    }

    fn proxy(&self) -> &zbus::Proxy<'static> {
        match self {
            Self::Fcitx5(proxy) | Self::Ibus(proxy) => proxy,
        }
    }

    async fn methods(&self, known: &[InnerMethod]) -> zbus::Result<Vec<InnerMethod>> {
        match self {
            Self::Fcitx5(controller) => fcitx_methods(controller).await,
            Self::Ibus(ibus) => ibus_methods(ibus, known).await,
        }
    }

    async fn current(&self) -> zbus::Result<Option<String>> {
        let current = match self {
            Self::Fcitx5(controller) => {
                controller
                    .call::<_, _, String>("CurrentInputMethod", &())
                    .await?
            }
            Self::Ibus(ibus) => {
                let desc: OwnedVariant = ibus.get_property("GlobalEngine").await?;
                engine_desc(&desc).map(|e| e.id).unwrap_or_default()
            }
        };
        Ok((!current.is_empty() && !is_layout(&current)).then_some(current))
    }

    async fn select(&self, id: &str) -> zbus::Result<()> {
        match self {
            Self::Fcitx5(controller) => controller.call_method("SetCurrentIM", &(id,)).await?,
            Self::Ibus(ibus) => ibus.call_method("SetGlobalEngine", &(id,)).await?,
        };
        Ok(())
    }
}

/// Keyboard layouts are not input methods for omimed: its direct input is the layout.
fn is_layout(id: &str) -> bool {
    id.starts_with("keyboard-") || id.starts_with("xkb:")
}

/// Whether names should be in Chinese.
fn chinese_locale() -> bool {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .is_ok_and(|locale| locale.starts_with("zh"))
}

/// The input methods of Fcitx 5's current group, in its order.
async fn fcitx_methods(controller: &zbus::Proxy<'_>) -> zbus::Result<Vec<InnerMethod>> {
    let group: String = controller.call("CurrentInputMethodGroup", &()).await?;
    let (_, items): (String, Vec<(String, String)>) =
        controller.call("InputMethodGroupInfo", &(group,)).await?;
    // (unique name, name, native name, icon, label, language, configurable)
    let available: Vec<(String, String, String, String, String, String, bool)> =
        controller.call("AvailableInputMethods", &()).await?;
    let known: HashMap<&str, _> = available.iter().map(|m| (m.0.as_str(), m)).collect();
    Ok(fcitx_list(&items, &known, chinese_locale()))
}

#[allow(clippy::type_complexity)]
fn fcitx_list(
    items: &[(String, String)],
    known: &HashMap<&str, &(String, String, String, String, String, String, bool)>,
    chinese: bool,
) -> Vec<InnerMethod> {
    items
        .iter()
        .map(|(id, _layout)| id)
        .filter(|id| !is_layout(id))
        .map(|id| {
            let Some((_, name, native, icon, label, ..)) = known.get(id.as_str()).copied() else {
                return InnerMethod {
                    id: id.clone(),
                    name: id.clone(),
                    ..InnerMethod::default()
                };
            };
            let name = if chinese && !native.is_empty() {
                native
            } else {
                name
            };
            InnerMethod {
                id: id.clone(),
                name: name.clone(),
                icon: icon.clone(),
                label: label.clone(),
            }
        })
        .collect()
}

/// The engines the user chose in IBus (its `preload-engines`), and the one in use.
///
/// Without a choice, IBus has no list worth showing: its `Engines` are every keyboard
/// layout, and reading them while IBus starts keeps its panel from ever showing
/// candidates. Then the engine in use is the list, and it grows as IBus switches.
async fn ibus_methods(
    ibus: &zbus::Proxy<'_>,
    known: &[InnerMethod],
) -> zbus::Result<Vec<InnerMethod>> {
    let mut names = preload_engines().unwrap_or_default();
    if names.is_empty() {
        names = known.iter().map(|m| m.id.clone()).collect();
        let current: OwnedVariant = ibus.get_property("GlobalEngine").await?;
        if let Some(current) = engine_desc(&current)
            && !names.contains(&current.id)
        {
            names.push(current.id);
        }
    }
    names.retain(|name| !is_layout(name));
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let descs: Vec<OwnedVariant> = ibus.call("GetEnginesByNames", &(names,)).await?;
    Ok(descs.iter().filter_map(engine_desc).collect())
}

/// IBus's `preload-engines` in the user's dconf database. The hosted IBus cannot reach
/// dconf's service, but the database is a file.
fn preload_engines() -> Option<Vec<String>> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let file = gvdb::read::File::from_file(&config.join("dconf/user")).ok()?;
    let table = file.hash_table().ok()?;
    table.get::<Vec<String>>(IBUS_PRELOAD_KEY).ok()
}

/// An `IBusEngineDesc`: ("IBusEngineDesc", attachments, name, long name, description,
/// language, license, author, icon, layout, rank, hotkeys, symbol, ...).
fn engine_desc(value: &OwnedVariant) -> Option<InnerMethod> {
    // `GlobalEngine` is a property of type `v`: its value comes in one variant more.
    let mut value: zbus::Variant<'_> = value.try_clone().ok()?.into();
    while let zbus::Variant::Variant(inner) = value {
        value = *inner;
    }
    let structure: Structure<'_> = value.try_into().ok()?;
    let fields = structure.fields();
    let text =
        |index: usize| -> Option<String> { fields.get(index)?.try_clone().ok()?.try_into().ok() };
    if text(0)? != "IBusEngineDesc" {
        return None;
    }
    let id = text(2)?;
    let long_name = text(3).filter(|n| !n.is_empty());
    Some(InnerMethod {
        name: long_name.unwrap_or_else(|| id.clone()),
        icon: text(8).unwrap_or_default(),
        label: text(12).unwrap_or_default(),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fcitx(
        id: &str,
        name: &str,
        native: &str,
        icon: &str,
        label: &str,
    ) -> (String, String, String, String, String, String, bool) {
        (
            id.into(),
            name.into(),
            native.into(),
            icon.into(),
            label.into(),
            "zh_CN".into(),
            true,
        )
    }

    #[test]
    fn fcitx_groups_become_input_methods() {
        let available = [
            fcitx(
                "keyboard-us",
                "Keyboard - English (US)",
                "",
                "input-keyboard",
                "en",
            ),
            fcitx("shuangpin", "Shuangpin", "双拼", "fcitx-shuangpin", "双"),
            fcitx("pinyin", "Pinyin", "拼音", "fcitx-pinyin", "拼"),
        ];
        let known: HashMap<&str, _> = available.iter().map(|m| (m.0.as_str(), m)).collect();
        let items: Vec<(String, String)> = ["keyboard-us", "shuangpin", "pinyin", "gone"]
            .iter()
            .map(|id| ((*id).to_owned(), String::new()))
            .collect();
        let chinese = fcitx_list(&items, &known, true);
        let ids: Vec<&str> = chinese.iter().map(|m| m.id.as_str()).collect();
        // The keyboard layout is direct input; an unknown input method keeps its id.
        assert_eq!(ids, ["shuangpin", "pinyin", "gone"]);
        assert_eq!(chinese[0].name, "双拼");
        assert_eq!(chinese[0].icon, "fcitx-shuangpin");
        assert_eq!(chinese[0].label, "双");
        assert_eq!(fcitx_list(&items, &known, false)[0].name, "Shuangpin");
    }

    #[test]
    fn ibus_engine_descriptions_are_read() {
        let desc = Structure::from((
            "IBusEngineDesc",
            HashMap::<String, OwnedVariant>::new(),
            "libpinyin",
            "Intelligent Pinyin",
            "Intelligent Pinyin input method",
            "zh_CN",
            "GPL",
            "Peng Wu",
            "/icons/ibus-pinyin.svg",
            "default",
            99u32,
            "",
            "拼",
            "",
            "",
            "",
        ));
        let value = OwnedVariant::try_from(zbus::Variant::from(desc)).expect("no fds");
        // As the `GlobalEngine` property gives it: in a variant.
        let wrapped = OwnedVariant::try_from(zbus::Variant::Variant(Box::new(
            value.try_clone().expect("no fds").into(),
        )))
        .expect("no fds");
        assert_eq!(engine_desc(&wrapped), engine_desc(&value));
        assert_eq!(
            engine_desc(&value),
            Some(InnerMethod {
                id: "libpinyin".into(),
                name: "Intelligent Pinyin".into(),
                icon: "/icons/ibus-pinyin.svg".into(),
                label: "拼".into(),
            })
        );
    }

    #[test]
    fn layouts_are_not_input_methods() {
        assert!(is_layout("keyboard-us"));
        assert!(is_layout("xkb:us::eng"));
        assert!(!is_layout("shuangpin"));
        assert!(!is_layout("libpinyin"));
    }
}
