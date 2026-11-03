//! omimed's tray item: an `org.kde.StatusNotifierItem` on the session bus, which the
//! tray of status bars (waybar, KDE, ...) shows.
//!
//! The icon is the current engine's. For a hosted input method it is the icon of the
//! input method's own tray item, which omimed follows on its private bus (see
//! [`crate::core::sni_watcher`]): Fcitx 5's "双拼", for example. The tooltip names the
//! engine and, when it says, what it is doing ("Fcitx 5 · 双拼"). A click switches
//! between direct input and the last engine, a middle click to the next engine, and the
//! menu (`com.canonical.dbusmenu`) lists the enabled engines and direct input, then the
//! current engine's own menu (switches such as Chinese punctuation), its settings and
//! oh-my-ime's settings.
//!
//! The item keeps no state of its own: every property is read from the daemon loop, and
//! the daemon tells the item when something changed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt as _;
use omime_proto::engine::{Property, PropertyKind, Status};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedObjectPath, Structure};
use zbus::{OwnedVariant, Variant};

use crate::control::ControlRequest;
use crate::core::sni_watcher::{HostedTray, Pixmap};
use crate::core::{EngineSummary, InputMethod, SwitchReason};

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
    /// The current engine's menu.
    pub properties: Vec<Property>,
    /// Whether oh-my-ime's settings program is installed.
    pub own_settings: bool,
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

    /// The menu: the input methods and direct input, to choose from, the current
    /// engine's own menu, then its settings if it has any. Input methods of the same name
    /// in two frameworks say which one they are in.
    fn menu(&self) -> Vec<MenuEntry> {
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
                MenuEntry::radio(MenuAction::Switch(Some(m.id.clone())), label, current)
            })
            .collect();
        entries.push(MenuEntry::radio(
            MenuAction::Switch(None),
            direct_input_label().to_owned(),
            self.engine.is_none(),
        ));
        let own = MenuEntry::from_properties(&self.properties, "", 0);
        if !own.is_empty() {
            entries.push(MenuEntry::separator());
            entries.extend(own);
        }
        let engine_settings = self.current().filter(|e| e.settings);
        if engine_settings.is_some() || self.own_settings {
            entries.push(MenuEntry::separator());
        }
        if let Some(engine) = engine_settings {
            entries.push(MenuEntry::item(
                MenuAction::Settings(engine.id.clone()),
                settings_label(&engine.name),
            ));
        }
        if self.own_settings {
            entries.push(MenuEntry::item(
                MenuAction::OwnSettings,
                own_settings_label().to_owned(),
            ));
        }
        entries
    }
}

/// What a menu entry does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MenuAction {
    /// Switches to an input method; `None` is direct input.
    Switch(Option<String>),
    /// Opens the settings of an engine.
    Settings(String),
    /// Opens oh-my-ime's settings.
    OwnSettings,
    /// Activates an entry of the current engine's menu.
    Property(String),
}

/// An entry of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuEntry {
    kind: EntryKind,
    label: String,
    /// An icon name.
    icon: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Separator,
    /// Does something when clicked, and may show whether it is on.
    Item {
        action: MenuAction,
        toggle: Option<Toggle>,
    },
    /// The submenu of the engine's menu entry `id`.
    Submenu {
        id: String,
        entries: Vec<MenuEntry>,
    },
}

/// How an entry shows whether it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    /// A switch.
    Checkmark(bool),
    /// One choice among several.
    Radio(bool),
}

impl MenuEntry {
    fn item(action: MenuAction, label: String) -> Self {
        Self {
            kind: EntryKind::Item {
                action,
                toggle: None,
            },
            label,
            icon: None,
            enabled: true,
        }
    }

    fn radio(action: MenuAction, label: String, checked: bool) -> Self {
        Self {
            kind: EntryKind::Item {
                action,
                toggle: Some(Toggle::Radio(checked)),
            },
            ..Self::item(MenuAction::Switch(None), label)
        }
    }

    fn separator() -> Self {
        Self {
            kind: EntryKind::Separator,
            label: String::new(),
            icon: None,
            enabled: true,
        }
    }

    /// The entries of an engine's menu that are in its entry `parent` (`""`: at the
    /// top). Entries whose parent is not a menu entry are left out.
    fn from_properties(properties: &[Property], parent: &str, depth: usize) -> Vec<Self> {
        // Engines that give two entries the same id could nest menus forever.
        if depth > 8 {
            return Vec::new();
        }
        properties
            .iter()
            .filter(|p| p.parent == parent)
            .map(|p| {
                let item = |toggle| EntryKind::Item {
                    action: MenuAction::Property(p.id.clone()),
                    toggle,
                };
                let kind = match p.kind {
                    PropertyKind::Separator => return Self::separator(),
                    PropertyKind::Action => item(None),
                    PropertyKind::Toggle => item(Some(Toggle::Checkmark(p.checked))),
                    PropertyKind::Radio => item(Some(Toggle::Radio(p.checked))),
                    PropertyKind::Menu => EntryKind::Submenu {
                        id: p.id.clone(),
                        entries: Self::from_properties(properties, &p.id, depth + 1),
                    },
                };
                Self {
                    kind,
                    label: p.label.clone(),
                    icon: p.icon.clone(),
                    enabled: p.enabled,
                }
            })
            .collect()
    }

    /// Its properties in `com.canonical.dbusmenu`.
    fn properties(&self) -> Properties {
        let text = |value: &str| owned(Variant::from(value));
        let mut properties = Properties::new();
        match &self.kind {
            EntryKind::Separator => {
                properties.insert("type".to_owned(), text("separator"));
                return properties;
            }
            EntryKind::Item {
                toggle: Some(toggle),
                ..
            } => {
                let (kind, on) = match *toggle {
                    Toggle::Checkmark(on) => ("checkmark", on),
                    Toggle::Radio(on) => ("radio", on),
                };
                properties.insert("toggle-type".to_owned(), text(kind));
                properties.insert(
                    "toggle-state".to_owned(),
                    owned(Variant::from(i32::from(on))),
                );
            }
            EntryKind::Item { toggle: None, .. } => {}
            EntryKind::Submenu { .. } => {
                properties.insert("children-display".to_owned(), text("submenu"));
            }
        }
        // An underscore marks the access key in dbusmenu labels.
        properties.insert("label".to_owned(), text(&self.label.replace('_', "__")));
        if let Some(icon) = &self.icon {
            properties.insert("icon-name".to_owned(), text(icon));
        }
        if !self.enabled {
            properties.insert("enabled".to_owned(), owned(Variant::from(false)));
        }
        properties
    }
}

/// Whether text for the user should be Chinese.
fn chinese() -> bool {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .is_ok_and(|locale| locale.starts_with("zh"))
}

/// "Direct input", in Chinese for Chinese locales.
fn direct_input_label() -> &'static str {
    if chinese() {
        "直接输入"
    } else {
        "Direct input"
    }
}

/// "Configure Fcitx 5…", in Chinese for Chinese locales.
fn settings_label(engine: &str) -> String {
    if chinese() {
        format!("配置 {engine}…")
    } else {
        format!("Configure {engine}…")
    }
}

/// "Settings…", oh-my-ime's own, in Chinese for Chinese locales.
fn own_settings_label() -> &'static str {
    if chinese() {
        "设置…"
    } else {
        "Settings…"
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
        tell(&self.requests, |tx| {
            ControlRequest::Toggle(SwitchReason::Menu, tx)
        });
    }

    /// A middle click: the next engine.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        tell(&self.requests, |tx| {
            ControlRequest::Next(SwitchReason::Menu, tx)
        });
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

/// The id of the root entry.
const ROOT: i32 = 0;

/// What an entry of the menu is, whatever else changes: the same entry keeps its id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EntryKey {
    Action(MenuAction),
    Submenu(String),
    /// The n-th separator.
    Separator(usize),
}

/// The ids of menu entries. An entry keeps its id while the menu changes, so a click on
/// a menu shown before a change still does what the entry said.
#[derive(Debug, Default)]
struct Ids {
    ids: HashMap<EntryKey, i32>,
    actions: HashMap<i32, MenuAction>,
    last: i32,
}

impl Ids {
    fn id(&mut self, key: EntryKey) -> i32 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        self.last += 1;
        if let EntryKey::Action(action) = &key {
            self.actions.insert(self.last, action.clone());
        }
        self.ids.insert(key, self.last);
        self.last
    }

    /// The menu's entries with their ids.
    fn nodes(&mut self, entries: &[MenuEntry], separators: &mut usize) -> Vec<Node> {
        entries
            .iter()
            .map(|entry| {
                let (key, children) = match &entry.kind {
                    EntryKind::Separator => {
                        *separators += 1;
                        (EntryKey::Separator(*separators), Vec::new())
                    }
                    EntryKind::Item { action, .. } => {
                        (EntryKey::Action(action.clone()), Vec::new())
                    }
                    EntryKind::Submenu { id, entries } => (
                        EntryKey::Submenu(id.clone()),
                        self.nodes(entries, separators),
                    ),
                };
                Node {
                    id: self.id(key),
                    properties: entry.properties(),
                    children,
                }
            })
            .collect()
    }
}

/// A menu entry with its id, and the entries of its submenu.
struct Node {
    id: i32,
    properties: Properties,
    children: Vec<Node>,
}

impl Node {
    fn find(&self, id: i32) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }

    /// The entry and all entries below it.
    fn all(&self) -> Vec<&Node> {
        std::iter::once(self)
            .chain(self.children.iter().flat_map(Node::all))
            .collect()
    }

    /// The layout of the entry with `depth` levels of entries below it (-1: all), with
    /// the properties in `names` (all if it is empty).
    fn layout(&self, depth: i32, names: &[String]) -> Layout {
        let children = if depth == 0 {
            Vec::new()
        } else {
            self.children
                .iter()
                .map(|child| {
                    owned(Variant::from(Structure::from(
                        child.layout(depth - 1, names),
                    )))
                })
                .collect()
        };
        (self.id, filtered(&self.properties, names), children)
    }
}

/// `properties` limited to `names`, unless it is empty.
fn filtered(properties: &Properties, names: &[String]) -> Properties {
    properties
        .iter()
        .filter(|(name, _)| names.is_empty() || names.contains(name))
        .filter_map(|(name, value)| Some((name.clone(), value.try_clone().ok()?)))
        .collect()
}

struct Menu {
    requests: mpsc::UnboundedSender<ControlRequest>,
    revision: Arc<AtomicU32>,
    ids: Mutex<Ids>,
}

impl Menu {
    /// The menu as it is now.
    async fn tree(&self) -> Node {
        let entries = ask(&self.requests, ControlRequest::Tray).await.menu();
        let mut ids = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        Node {
            id: ROOT,
            properties: Properties::from([(
                "children-display".to_owned(),
                owned(Variant::from("submenu")),
            )]),
            children: ids.nodes(&entries, &mut 0),
        }
    }

    fn clicked(&self, id: i32) {
        let action = self
            .ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .actions
            .get(&id)
            .cloned();
        match action {
            Some(MenuAction::Switch(target)) => {
                tell(&self.requests, |tx| {
                    ControlRequest::Switch(target, SwitchReason::Menu, tx)
                });
            }
            Some(MenuAction::Settings(engine)) => {
                tell(&self.requests, |tx| {
                    ControlRequest::OpenSettings(engine, tx)
                });
            }
            Some(MenuAction::OwnSettings) => {
                tell(&self.requests, ControlRequest::OpenOwnSettings);
            }
            Some(MenuAction::Property(property)) => {
                tell(&self.requests, |tx| {
                    ControlRequest::ActivateProperty(property, tx)
                });
            }
            None => {}
        }
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    async fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let revision = self.revision.load(Ordering::Relaxed);
        let tree = self.tree().await;
        let parent = tree
            .find(parent_id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no menu entry {parent_id}")))?;
        Ok((revision, parent.layout(recursion_depth, &property_names)))
    }

    async fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let tree = self.tree().await;
        // No ids: all entries.
        tree.all()
            .into_iter()
            .filter(|node| node.id != ROOT && (ids.is_empty() || ids.contains(&node.id)))
            .map(|node| (node.id, filtered(&node.properties, &property_names)))
            .collect()
    }

    async fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedVariant> {
        self.tree()
            .await
            .find(id)
            .and_then(|node| node.properties.get(&name)?.try_clone().ok())
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
                ids: Mutex::new(Ids::default()),
            },
        )
        .build()
        .await?;
    // A new name each time: the connection of an item turned off and on again may not
    // be gone yet.
    static ITEMS: AtomicU32 = AtomicU32::new(0);
    let item_number = ITEMS.fetch_add(1, Ordering::Relaxed) + 1;
    let name = format!(
        "org.kde.StatusNotifierItem-{}-{item_number}",
        std::process::id()
    );
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
            settings: id == "fcitx5",
            description: None,
            languages: Vec::new(),
            hosted: id == "fcitx5" || id == "ibus",
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
            language: None,
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
        let switch = |id: Option<&str>, label: &str, checked: bool| {
            MenuEntry::radio(
                MenuAction::Switch(id.map(str::to_owned)),
                label.to_owned(),
                checked,
            )
        };
        let choices = [
            switch(Some("fcitx5/shuangpin"), "双拼", false),
            // Two frameworks have a 拼音: each says which it is.
            switch(Some("fcitx5/pinyin"), "拼音 (Fcitx 5)", true),
            switch(Some("ibus/pinyin"), "拼音 (IBus)", false),
            switch(Some("sparkle"), "Sparkle", false),
            switch(None, direct_input_label(), false),
        ];
        // Fcitx 5 has settings, which the menu offers while it is in use.
        let menu = info(Some("fcitx5"), Some("fcitx5/pinyin")).menu();
        assert_eq!(menu[..5], choices);
        assert_eq!(menu[5], MenuEntry::separator());
        assert_eq!(
            menu[6],
            MenuEntry::item(
                MenuAction::Settings("fcitx5".to_owned()),
                settings_label("Fcitx 5")
            )
        );
        assert_eq!(menu.len(), 7);
        let menu = info(Some("sparkle"), Some("sparkle")).menu();
        assert_eq!(menu.len(), 5);

        // oh-my-ime's settings come last, where they are installed.
        let mut fcitx5 = info(Some("fcitx5"), Some("fcitx5/pinyin"));
        fcitx5.own_settings = true;
        let menu = fcitx5.menu();
        assert_eq!(menu.len(), 8);
        assert_eq!(
            menu[7],
            MenuEntry::item(MenuAction::OwnSettings, own_settings_label().to_owned())
        );
        let mut direct = info(None, None);
        direct.own_settings = true;
        let menu = direct.menu();
        assert_eq!(menu[menu.len() - 2], MenuEntry::separator());
    }

    fn property(id: &str, parent: &str, kind: PropertyKind, checked: bool) -> Property {
        Property {
            id: id.to_owned(),
            parent: parent.to_owned(),
            kind,
            label: id.to_uppercase(),
            checked,
            ..Property::default()
        }
    }

    /// A menu like a Rime engine's.
    fn engine_menu() -> Vec<Property> {
        vec![
            property("punct", "", PropertyKind::Toggle, true),
            property("width", "", PropertyKind::Menu, false),
            property("full", "width", PropertyKind::Radio, false),
            property("half", "width", PropertyKind::Radio, true),
            property("sep", "", PropertyKind::Separator, false),
            Property {
                enabled: false,
                icon: Some("view-refresh".to_owned()),
                ..property("deploy", "", PropertyKind::Action, false)
            },
            // Not in any menu.
            property("lost", "nowhere", PropertyKind::Action, false),
        ]
    }

    #[test]
    fn the_menu_has_the_engines_own_menu() {
        let mut info = info(Some("sparkle"), Some("sparkle"));
        info.properties = engine_menu();
        let menu = info.menu();
        let own = &menu[5..];
        let property = |id: &str, toggle| MenuEntry {
            kind: EntryKind::Item {
                action: MenuAction::Property(id.to_owned()),
                toggle,
            },
            label: id.to_uppercase(),
            icon: None,
            enabled: true,
        };
        assert_eq!(own[0], MenuEntry::separator());
        assert_eq!(own[1], property("punct", Some(Toggle::Checkmark(true))));
        assert_eq!(
            own[2],
            MenuEntry {
                kind: EntryKind::Submenu {
                    id: "width".to_owned(),
                    entries: vec![
                        property("full", Some(Toggle::Radio(false))),
                        property("half", Some(Toggle::Radio(true))),
                    ],
                },
                ..property("width", None)
            }
        );
        assert_eq!(own[3], MenuEntry::separator());
        assert_eq!(
            own[4],
            MenuEntry {
                icon: Some("view-refresh".to_owned()),
                enabled: false,
                ..property("deploy", None)
            }
        );
        assert_eq!(own.len(), 5);

        let deploy = own[4].properties();
        let text = |properties: &Properties, name: &str| {
            properties
                .get(name)
                .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        };
        assert_eq!(text(&deploy, "icon-name").as_deref(), Some("view-refresh"));
        assert!(deploy.contains_key("enabled"));
        assert_eq!(
            text(&own[1].properties(), "toggle-type").as_deref(),
            Some("checkmark")
        );
        assert_eq!(
            text(&own[2].properties(), "children-display").as_deref(),
            Some("submenu")
        );
        // dbusmenu would underline the character after an underscore.
        let entry = MenuEntry::item(MenuAction::Switch(None), "rime_ice".to_owned());
        assert_eq!(
            text(&entry.properties(), "label").as_deref(),
            Some("rime__ice")
        );
    }

    #[test]
    fn menu_entries_keep_their_ids() {
        let mut info = info(Some("sparkle"), Some("sparkle"));
        info.properties = engine_menu();
        let mut ids = Ids::default();
        let before = ids.nodes(&info.menu(), &mut 0);
        // Another input method comes first.
        info.methods
            .insert(0, method("fcitx5/rime", "Rime", "fcitx5", "Fcitx 5"));
        let after = ids.nodes(&info.menu(), &mut 0);
        assert_eq!(after.len(), before.len() + 1);
        for (old, new) in before.iter().zip(&after[1..]) {
            assert_eq!(old.id, new.id);
        }
        let half = after[8].children[1].id;
        assert_eq!(
            ids.actions.get(&half),
            Some(&MenuAction::Property("half".to_owned()))
        );
        assert!(!after.iter().any(|node| node.id == ROOT));

        // Layouts go as deep as asked.
        let tree = Node {
            id: ROOT,
            properties: Properties::new(),
            children: after,
        };
        let depth = |layout: &Layout| -> usize {
            fn depth(layout: &Layout) -> usize {
                layout
                    .2
                    .iter()
                    .filter_map(|child| Layout::try_from(child.try_clone().ok()?).ok())
                    .map(|child| 1 + depth(&child))
                    .max()
                    .unwrap_or(0)
            }
            depth(layout)
        };
        assert_eq!(depth(&tree.layout(-1, &[])), 2);
        assert_eq!(depth(&tree.layout(1, &[])), 1);
        assert_eq!(depth(&tree.layout(0, &[])), 0);
        let width = tree.find(tree.children[8].id).expect("the submenu");
        assert_eq!(width.layout(-1, &[]).2.len(), 2);
        let labels = tree.layout(1, &["label".to_owned()]);
        let first = Layout::try_from(labels.2[0].try_clone().unwrap()).unwrap();
        assert_eq!(first.1.keys().collect::<Vec<_>>(), ["label"]);
    }
}
