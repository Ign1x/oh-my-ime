//! omimed's tray item: an `org.kde.StatusNotifierItem` on the session bus, which the
//! tray of status bars (waybar, KDE, ...) shows.
//!
//! The icon is the current engine's. For a hosted input method it is the icon of the
//! input method's own tray item, which omimed follows on its private bus (see
//! [`crate::core::sni_watcher`]): Fcitx 5's "双拼", for example. The tooltip names the
//! engine and, when it says, what it is doing ("Fcitx 5 · 双拼"). A click switches
//! between direct input and the last engine, a middle click to the next engine, and the
//! menu (`com.canonical.dbusmenu`) lists the enabled engines and direct input.
//!
//! The item keeps no state of its own: every property is read from the daemon loop, and
//! the daemon tells the item when something changed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;
use omime_proto::engine::Status;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedObjectPath, Structure};
use zbus::{OwnedVariant, Variant};

use crate::control::ControlRequest;
use crate::core::sni_watcher::{HostedTray, Pixmap};
use crate::core::{EngineSummary, InputMethod};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
/// The icon of direct input, and of engines that name none.
const KEYBOARD_ICON: &str = "input-keyboard";

/// What the tray item shows, as the daemon loop knows it.
#[derive(Debug, Default)]
pub struct TrayInfo {
    /// The current engine; `None` is direct input.
    pub engine: Option<String>,
    /// All installed engines, enabled ones first in switching order.
    pub engines: Vec<EngineSummary>,
    /// The status the current engine published.
    pub status: Status,
    /// What the tray item of the current engine shows, for hosted engines.
    pub hosted: Option<HostedTray>,
    /// What users choose from, in switching order.
    pub methods: Vec<InputMethod>,
    /// The input method in use; `None` is direct input.
    pub method: Option<String>,
}

impl TrayInfo {
    fn current(&self) -> Option<&EngineSummary> {
        let engine = self.engine.as_deref()?;
        self.engines.iter().find(|e| e.id == engine)
    }

    fn hosted_icon(&self) -> Option<&HostedTray> {
        self.hosted.as_ref().filter(|tray| tray.has_icon())
    }

    fn icon_name(&self) -> String {
        if let Some(tray) = self.hosted_icon() {
            return tray.icon_name.clone();
        }
        self.status
            .icon
            .clone()
            .or_else(|| self.current().and_then(|e| e.icon.clone()))
            .unwrap_or_else(|| KEYBOARD_ICON.to_owned())
    }

    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.hosted_icon()
            .map(|tray| tray.icon_pixmap.clone())
            .unwrap_or_default()
    }

    fn icon_theme_path(&self) -> String {
        self.hosted_icon()
            .map(|tray| tray.icon_theme_path.clone())
            .unwrap_or_default()
    }

    /// The input method in use, if it is one inside a hosted framework.
    fn inner_method(&self) -> Option<&InputMethod> {
        let method = self.method.as_deref()?;
        self.methods
            .iter()
            .find(|m| m.id == method && m.id != m.engine)
    }

    /// "双拼 (Fcitx 5)" for an input method inside a framework; for an engine of
    /// omimed's, "Sparkle · emoji": the engine and what it says it is doing.
    fn title(&self) -> String {
        if let Some(method) = self.inner_method() {
            return format!("{} ({})", method.name, method.engine_name);
        }
        let Some(engine) = self.current() else {
            return direct_input_label().to_owned();
        };
        let detail = self
            .status
            .tooltip
            .as_deref()
            .or(self.status.label.as_deref())
            .filter(|detail| !detail.is_empty() && *detail != engine.name);
        match detail {
            Some(detail) => format!("{} · {detail}", engine.name),
            None => engine.name.clone(),
        }
    }

    fn label(&self) -> String {
        self.status.label.clone().unwrap_or_default()
    }

    /// The menu entries: the input methods, then direct input. `None` is direct input.
    /// Input methods of the same name in two frameworks say which one they are in.
    fn menu(&self) -> Vec<(Option<String>, String, bool)> {
        let mut entries: Vec<_> = self
            .methods
            .iter()
            .map(|m| {
                let twins = self.methods.iter().filter(|o| o.name == m.name).count();
                let label = if twins > 1 {
                    format!("{} ({})", m.name, m.engine_name)
                } else {
                    m.name.clone()
                };
                let current = self.method.as_deref() == Some(m.id.as_str());
                (Some(m.id.clone()), label, current)
            })
            .collect();
        entries.push((None, direct_input_label().to_owned(), self.engine.is_none()));
        entries
    }
}

/// "Direct input", in Chinese for Chinese locales.
fn direct_input_label() -> &'static str {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    if locale.starts_with("zh") {
        "直接输入"
    } else {
        "Direct input"
    }
}

/// Asks the daemon loop.
async fn ask<T: Default>(
    requests: &mpsc::UnboundedSender<ControlRequest>,
    make: impl FnOnce(oneshot::Sender<T>) -> ControlRequest,
) -> T {
    let (tx, rx) = oneshot::channel();
    if requests.send(make(tx)).is_err() {
        return T::default();
    }
    rx.await.unwrap_or_default()
}

/// Tells the daemon loop to do something; the answer does not matter.
fn tell(
    requests: &mpsc::UnboundedSender<ControlRequest>,
    make: impl FnOnce(oneshot::Sender<Result<(), String>>) -> ControlRequest,
) {
    let (tx, _rx) = oneshot::channel();
    let _ = requests.send(make(tx));
}

struct Item {
    requests: mpsc::UnboundedSender<ControlRequest>,
}

impl Item {
    async fn info(&self) -> TrayInfo {
        ask(&self.requests, ControlRequest::Tray).await
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    /// The menu is shown by the host, from `Menu`.
    fn context_menu(&self, _x: i32, _y: i32) {}

    /// A click: direct input ↔ the last engine.
    fn activate(&self, _x: i32, _y: i32) {
        tell(&self.requests, ControlRequest::Toggle);
    }

    /// A middle click: the next engine.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        tell(&self.requests, ControlRequest::Next);
    }

    fn scroll(&self, _delta: i32, _orientation: String) {}

    #[zbus(property, name = "Category")]
    fn category(&self) -> String {
        "SystemServices".to_owned()
    }

    #[zbus(property, name = "Id")]
    fn id(&self) -> String {
        "oh-my-ime".to_owned()
    }

    #[zbus(property, name = "Title")]
    fn title(&self) -> String {
        "oh-my-ime".to_owned()
    }

    #[zbus(property, name = "Status")]
    fn status(&self) -> String {
        "Active".to_owned()
    }

    #[zbus(property, name = "WindowId")]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property, name = "IconName")]
    async fn icon_name(&self) -> String {
        self.info().await.icon_name()
    }

    #[zbus(property, name = "IconPixmap")]
    async fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.info().await.icon_pixmap()
    }

    #[zbus(property, name = "IconThemePath")]
    async fn icon_theme_path(&self) -> String {
        self.info().await.icon_theme_path()
    }

    #[zbus(property, name = "OverlayIconName")]
    fn overlay_icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property, name = "OverlayIconPixmap")]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property, name = "AttentionIconName")]
    fn attention_icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property, name = "AttentionIconPixmap")]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property, name = "AttentionMovieName")]
    fn attention_movie_name(&self) -> String {
        String::new()
    }

    #[zbus(property, name = "ToolTip")]
    async fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let info = self.info().await;
        (
            info.icon_name(),
            info.icon_pixmap(),
            info.title(),
            String::new(),
        )
    }

    #[zbus(property, name = "ItemIsMenu")]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property, name = "Menu")]
    fn menu(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(MENU_PATH).into()
    }

    #[zbus(property, name = "XAyatanaLabel")]
    async fn x_ayatana_label(&self) -> String {
        self.info().await.label()
    }

    #[zbus(property, name = "XAyatanaLabelGuide")]
    async fn x_ayatana_label_guide(&self) -> String {
        self.info().await.label()
    }

    #[zbus(signal, name = "NewTitle")]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewIcon")]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewToolTip")]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewStatus")]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "NewIconThemePath")]
    async fn new_icon_theme_path(emitter: &SignalEmitter<'_>, path: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "XAyatanaNewLabel")]
    async fn x_ayatana_new_label(
        emitter: &SignalEmitter<'_>,
        label: &str,
        guide: &str,
    ) -> zbus::Result<()>;
}

/// Properties of a menu entry.
type Properties = HashMap<String, OwnedVariant>;
/// A menu entry with its children, `(ia{sv}av)`: the children are entries in variants.
type Layout = (i32, Properties, Vec<OwnedVariant>);

fn owned(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("menu values have no file descriptors")
}

/// The id of the root entry; entry `i` of [`TrayInfo::menu`] has the id `i + 1`.
const ROOT: i32 = 0;

struct Menu {
    requests: mpsc::UnboundedSender<ControlRequest>,
    revision: Arc<AtomicU32>,
    /// What the entries of the layout last handed out switch to.
    targets: Mutex<Vec<Option<String>>>,
}

impl Menu {
    /// The entries, by id. Remembers what each one switches to.
    async fn entries(&self) -> Vec<(i32, Properties)> {
        let entries = ask(&self.requests, ControlRequest::Tray).await.menu();
        if let Ok(mut targets) = self.targets.lock() {
            *targets = entries.iter().map(|(target, ..)| target.clone()).collect();
        }
        entries
            .into_iter()
            .zip(1..)
            .map(|((_, label, current), id)| {
                let properties = Properties::from([
                    ("label".to_owned(), owned(Variant::from(label))),
                    ("toggle-type".to_owned(), owned(Variant::from("radio"))),
                    (
                        "toggle-state".to_owned(),
                        owned(Variant::from(i32::from(current))),
                    ),
                ]);
                (id, properties)
            })
            .collect()
    }

    fn root() -> Properties {
        Properties::from([(
            "children-display".to_owned(),
            owned(Variant::from("submenu")),
        )])
    }

    /// The properties of `id`, limited to `names` unless it is empty.
    async fn properties(&self, id: i32, names: &[String]) -> Option<Properties> {
        let mut properties = if id == ROOT {
            Self::root()
        } else {
            self.entries()
                .await
                .into_iter()
                .find(|(entry, _)| *entry == id)?
                .1
        };
        if !names.is_empty() {
            properties.retain(|name, _| names.contains(name));
        }
        Some(properties)
    }

    fn clicked(&self, id: i32) {
        let target = self.targets.lock().ok().and_then(|targets| {
            usize::try_from(id - 1)
                .ok()
                .and_then(|index| targets.get(index).cloned())
        });
        if let Some(target) = target {
            tell(&self.requests, |tx| ControlRequest::Switch(target, tx));
        }
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    async fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let revision = self.revision.load(Ordering::Relaxed);
        let filter = |mut properties: Properties| {
            if !property_names.is_empty() {
                properties.retain(|name, _| property_names.contains(name));
            }
            properties
        };
        let entries = self.entries().await;
        if parent_id == ROOT {
            let children = entries
                .into_iter()
                .map(|(id, properties)| {
                    let entry: Layout = (id, filter(properties), Vec::new());
                    owned(Variant::from(Structure::from(entry)))
                })
                .collect();
            return Ok((revision, (ROOT, filter(Self::root()), children)));
        }
        let (id, properties) = entries
            .into_iter()
            .find(|(id, _)| *id == parent_id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no menu entry {parent_id}")))?;
        Ok((revision, (id, filter(properties), Vec::new())))
    }

    async fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let mut result = Vec::new();
        for id in ids {
            if let Some(properties) = self.properties(id, &property_names).await {
                result.push((id, properties));
            }
        }
        result
    }

    async fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedVariant> {
        self.properties(id, std::slice::from_ref(&name))
            .await
            .and_then(|mut properties| properties.remove(&name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} of {id}")))
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedVariant, _timestamp: u32) {
        if event_id == "clicked" {
            self.clicked(id);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedVariant, u32)>) -> Vec<i32> {
        for (id, event_id, _, _) in events {
            if event_id == "clicked" {
                self.clicked(id);
            }
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(property, name = "Version")]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property, name = "TextDirection")]
    fn text_direction(&self) -> String {
        "ltr".to_owned()
    }

    #[zbus(property, name = "Status")]
    fn status(&self) -> String {
        "normal".to_owned()
    }

    #[zbus(property, name = "IconThemePath")]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(signal, name = "ItemsPropertiesUpdated")]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: Vec<(i32, Properties)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "LayoutUpdated")]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "ItemActivationRequested")]
    async fn item_activation_requested(
        emitter: &SignalEmitter<'_>,
        id: i32,
        timestamp: u32,
    ) -> zbus::Result<()>;
}

/// The running tray item.
#[derive(Debug)]
pub struct TrayService {
    changed: mpsc::UnboundedSender<()>,
}

impl TrayService {
    /// The current engine, its status or the engines changed.
    pub fn changed(&self) {
        let _ = self.changed.send(());
    }
}

/// Publishes the tray item on the session bus and registers it with the status bar's
/// watcher, now and whenever a watcher appears.
pub async fn start(requests: mpsc::UnboundedSender<ControlRequest>) -> zbus::Result<TrayService> {
    let revision = Arc::new(AtomicU32::new(1));
    let conn = zbus::connection::Builder::session()
        .serve_at(
            ITEM_PATH,
            Item {
                requests: requests.clone(),
            },
        )
        .serve_at(
            MENU_PATH,
            Menu {
                requests,
                revision: revision.clone(),
                targets: Mutex::new(Vec::new()),
            },
        )
        .build()
        .await?;
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    conn.request_name(name.as_str()).await?;

    let (changed, mut changes) = mpsc::unbounded_channel();
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut watchers = dbus
        .receive_name_owner_changed_with_args(&[(0, WATCHER_NAME)])
        .await?;
    tokio::task::spawn_local(async move {
        register(&conn, &name).await;
        let item = SignalEmitter::new(&conn, ITEM_PATH).expect("a valid object path");
        let menu = SignalEmitter::new(&conn, MENU_PATH).expect("a valid object path");
        loop {
            tokio::select! {
                change = changes.recv() => {
                    if change.is_none() {
                        break;
                    }
                    // One update for a burst of changes.
                    while changes.try_recv().is_ok() {}
                    let _ = Item::new_icon(&item).await;
                    let _ = Item::new_tool_tip(&item).await;
                    let _ = Item::new_icon_theme_path(&item, "").await;
                    let _ = Item::x_ayatana_new_label(&item, "", "").await;
                    let revision = revision.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = Menu::layout_updated(&menu, revision, ROOT).await;
                }
                owner = watchers.next() => {
                    let Some(owner) = owner else { break };
                    let appeared = owner
                        .args()
                        .is_ok_and(|args| args.new_owner().is_some());
                    if appeared {
                        register(&conn, &name).await;
                    }
                }
            }
        }
    });
    Ok(TrayService { changed })
}

/// Registers the item with the status bar's watcher, if one runs.
async fn register(conn: &zbus::Connection, name: &str) {
    let result = conn
        .call_method(
            Some(WATCHER_NAME),
            WATCHER_PATH,
            Some(WATCHER_NAME),
            "RegisterStatusNotifierItem",
            &(name,),
        )
        .await;
    match result {
        Ok(_) => info!("the tray item is registered"),
        Err(e) => debug!("no tray to show the tray item in yet: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(id: &str, name: &str, enabled: bool, icon: Option<&str>) -> EngineSummary {
        EngineSummary {
            id: id.to_owned(),
            name: name.to_owned(),
            enabled,
            state: "running",
            icon: icon.map(str::to_owned),
        }
    }

    fn method(id: &str, name: &str, engine: &str, engine_name: &str) -> InputMethod {
        InputMethod {
            id: id.to_owned(),
            name: name.to_owned(),
            engine: engine.to_owned(),
            engine_name: engine_name.to_owned(),
            icon: None,
            label: None,
        }
    }

    fn info(current: Option<&str>, method_in_use: Option<&str>) -> TrayInfo {
        TrayInfo {
            engine: current.map(str::to_owned),
            engines: vec![
                engine("fcitx5", "Fcitx 5", true, None),
                engine("ibus", "IBus", true, None),
                engine("sparkle", "Sparkle", true, Some("sparkle-icon")),
                engine("echo", "Echo", false, None),
            ],
            methods: vec![
                method("fcitx5/shuangpin", "双拼", "fcitx5", "Fcitx 5"),
                method("fcitx5/pinyin", "拼音", "fcitx5", "Fcitx 5"),
                method("ibus/pinyin", "拼音", "ibus", "IBus"),
                method("sparkle", "Sparkle", "sparkle", "Sparkle"),
            ],
            method: method_in_use.map(str::to_owned),
            ..TrayInfo::default()
        }
    }

    #[test]
    fn direct_input_shows_a_keyboard() {
        let info = info(None, None);
        assert_eq!(info.icon_name(), KEYBOARD_ICON);
        assert_eq!(info.title(), direct_input_label());
    }

    #[test]
    fn engines_show_their_icon_and_status() {
        let mut info = info(Some("sparkle"), Some("sparkle"));
        assert_eq!(info.icon_name(), "sparkle-icon");
        assert_eq!(info.title(), "Sparkle");
        info.status.tooltip = Some("emoji".to_owned());
        info.status.icon = Some("sparkle-busy".to_owned());
        assert_eq!(info.icon_name(), "sparkle-busy");
        assert_eq!(info.title(), "Sparkle · emoji");
    }

    #[test]
    fn input_methods_inside_frameworks_show_their_own_icon_and_name() {
        let mut info = info(Some("fcitx5"), Some("fcitx5/shuangpin"));
        assert_eq!(info.icon_name(), KEYBOARD_ICON);
        info.hosted = Some(HostedTray {
            icon_name: "fcitx-shuangpin".to_owned(),
            icon_theme_path: "/fcitx/share/icons".to_owned(),
            title: "Shuangpin".to_owned(),
            ..HostedTray::default()
        });
        assert_eq!(info.icon_name(), "fcitx-shuangpin");
        assert_eq!(info.icon_theme_path(), "/fcitx/share/icons");
        assert_eq!(info.title(), "双拼 (Fcitx 5)");
    }

    #[test]
    fn the_menu_lists_input_methods_and_direct_input() {
        let menu = info(Some("fcitx5"), Some("fcitx5/pinyin")).menu();
        let entries: Vec<(Option<&str>, &str, bool)> = menu
            .iter()
            .map(|(id, name, current)| (id.as_deref(), name.as_str(), *current))
            .collect();
        assert_eq!(
            entries,
            [
                (Some("fcitx5/shuangpin"), "双拼", false),
                // Two frameworks have a 拼音: each says which it is.
                (Some("fcitx5/pinyin"), "拼音 (Fcitx 5)", true),
                (Some("ibus/pinyin"), "拼音 (IBus)", false),
                (Some("sparkle"), "Sparkle", false),
                (None, direct_input_label(), false),
            ]
        );
    }
}
