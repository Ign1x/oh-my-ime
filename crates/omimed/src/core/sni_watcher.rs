//! A tray host for hosted input methods.
//!
//! Hosted input methods such as Fcitx 5 and IBus publish a tray item
//! (`org.kde.StatusNotifierItem`) that shows their current input method, for example
//! Fcitx 5's "双拼". They run on a private bus, where no status bar can take the item.
//! omimed plays the `org.kde.StatusNotifierWatcher` there, follows the item the input
//! method registers, and reports what it shows; omimed's own tray item shows it while
//! that engine is current.
//!
//! The item's menu (`com.canonical.dbusmenu`) becomes the engine's menu: the switches
//! and choices of the input method, such as Fcitx 5's full width and Chinese
//! punctuation, without the input methods and the framework's own entries, which
//! omimed has in its own menu. Clicks on them go back to the item's menu.
//!
//! IBus (1.5.34) exports its menu anew only when an engine registers its properties,
//! not when one of them changes: after a click switches libpinyin to English, its menu
//! still says "Chinese". omimed follows the engines' `UpdateProperty` on IBus's bus,
//! and has the panel build its menu afresh after them: a middle click on IBus's item
//! flips its menu to the panel's own and builds it, and a second one flips it back.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::{FutureExt as _, StreamExt as _};
use omime_proto::engine::{Property, PropertyKind};
use tokio::sync::mpsc;
use tracing::debug;
use zbus::message::Header;
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use super::CoreEvent;

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
/// How deep menus go, at most.
const MENU_DEPTH: usize = 8;
/// How long after an IBus engine changed its properties its menu is built again: the
/// panel learns of the change from IBus, on another connection than omimed's.
const IBUS_MENU_DELAY: Duration = Duration::from_millis(200);

/// An icon as `IconPixmap` carries it: width, height and ARGB32 data in network byte
/// order.
pub type Pixmap = (i32, i32, Vec<u8>);

/// What a hosted input method's tray item shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostedTray {
    /// An icon name, or the path of an icon file.
    pub icon_name: String,
    pub icon_pixmap: Vec<Pixmap>,
    /// Where to look for `icon_name` besides the icon theme.
    pub icon_theme_path: String,
    /// The tooltip's title: for Fcitx 5, the name of the current input method.
    pub title: String,
    /// `XAyatanaLabel`: a short label, such as IBus engines' symbols.
    pub label: String,
}

impl HostedTray {
    /// Whether the item shows an icon at all.
    pub fn has_icon(&self) -> bool {
        !self.icon_name.is_empty() || !self.icon_pixmap.is_empty()
    }
}

/// The menu of a hosted input method's tray item, as the engine's menu.
#[derive(Debug, Clone)]
pub struct HostedMenu {
    /// The entries, with the menu's ids for ids.
    pub properties: Vec<Property>,
    /// Takes the ids of entries to click.
    pub clicks: mpsc::UnboundedSender<i32>,
}

/// Whose menu it is, which says what of it is the input method's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    /// Fcitx 5: its status actions have ids from 300 on, after its own entries (1-99),
    /// its input methods (100-199) and groups (200-299).
    Fcitx5,
    /// IBus: the engine's properties come first, up to the first separator; then its
    /// engines, Preferences and so on.
    Ibus,
    /// Anything else: the whole menu.
    Other,
}

impl MenuKind {
    /// After the item's `Id`.
    fn of(id: &str) -> Self {
        match id {
            "Fcitx" => Self::Fcitx5,
            id if id.to_ascii_lowercase().starts_with("ibus") => Self::Ibus,
            _ => Self::Other,
        }
    }
}

/// The watcher of one hosted input method. Dropping it stops following the item and
/// leaves the private bus.
pub struct TrayWatcher {
    _conn: zbus::Connection,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TrayWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for TrayWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayWatcher").finish_non_exhaustive()
    }
}

struct Watcher {
    items: mpsc::UnboundedSender<(String, String)>,
    registered: Mutex<Vec<String>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // KDE's protocol takes a bus name; libappindicator and IBus pass an object path
        // of the caller.
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let (name, path) = if service.starts_with('/') {
            (sender, service)
        } else {
            (service, ITEM_PATH.to_owned())
        };
        let id = format!("{name}{path}");
        if let Ok(mut registered) = self.registered.lock() {
            registered.retain(|item| item != &id);
            registered.push(id.clone());
        }
        let _ = Self::status_notifier_item_registered(&emitter, &id).await;
        let _ = self.items.send((name, path));
    }

    fn register_status_notifier_host(&self, _service: String) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registered
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    /// Items only register while a host is there to show them.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Plays the watcher on the private bus at `address` of the hosted engine `engine`
/// (launch `generation`), and reports its tray item as [`CoreEvent::HostedTray`] and
/// its menu as [`CoreEvent::HostedMenu`]. `icon_dir` is where the input method's icons
/// are when its item does not say; `runtime_dir` is the engine's runtime directory,
/// where `omime-ibus` says where IBus is.
pub async fn start(
    engine: String,
    generation: u64,
    address: &str,
    runtime_dir: PathBuf,
    icon_dir: Option<String>,
    events: mpsc::UnboundedSender<CoreEvent>,
) -> zbus::Result<TrayWatcher> {
    let (items, mut registrations) = mpsc::unbounded_channel();
    let conn = zbus::connection::Builder::address(address)
        .serve_at(
            WATCHER_PATH,
            Watcher {
                items,
                registered: Mutex::new(Vec::new()),
            },
        )
        .name(WATCHER_NAME)
        .build()
        .await?;
    let follower = conn.clone();
    let task = tokio::task::spawn_local(async move {
        let report = move |update: Update| {
            let event = match update {
                Update::Tray(tray) => CoreEvent::HostedTray {
                    engine: engine.clone(),
                    generation,
                    tray,
                },
                Update::Menu(menu) => CoreEvent::HostedMenu {
                    engine: engine.clone(),
                    generation,
                    menu,
                },
            };
            let _ = events.send(event);
        };
        // Only the item registered last is followed.
        let mut following: Pin<Box<dyn Future<Output = ()>>> = Box::pin(std::future::pending());
        loop {
            tokio::select! {
                item = registrations.recv() => {
                    let Some((name, path)) = item else { break };
                    debug!(name, path, "a hosted input method registered its tray item");
                    following = Box::pin(follow(
                        follower.clone(),
                        name,
                        path,
                        icon_dir.clone(),
                        runtime_dir.clone(),
                        report.clone(),
                    ));
                }
                () = &mut following => following = Box::pin(std::future::pending()),
            }
        }
    });
    Ok(TrayWatcher { _conn: conn, task })
}

/// What changed about a tray item.
enum Update {
    Tray(Option<HostedTray>),
    Menu(Option<HostedMenu>),
}

/// A menu entry with its children, `(ia{sv}av)`: the children are entries in variants.
type Layout = (i32, HashMap<String, OwnedVariant>, Vec<OwnedVariant>);

/// Reports the item `name` `path` and its menu now and whenever they change, until the
/// item goes away, and clicks entries of the menu.
async fn follow(
    conn: zbus::Connection,
    name: String,
    path: String,
    icon_dir: Option<String>,
    runtime_dir: PathBuf,
    report: impl Fn(Update),
) {
    let result: zbus::Result<()> = async {
        let item: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
            .destination(name.as_str())
            .path(path.as_str())
            .interface(ITEM_INTERFACE)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let properties = zbus::fdo::PropertiesProxy::builder(&conn)
            .destination(name.as_str())
            .path(path.as_str())
            .build()
            .await?;
        // Every signal of the item (NewIcon, NewToolTip, ...) says something changed.
        let mut changes = item.receive_all_signals().await?;
        let mut owner = item.receive_owner_changed().await?;
        let interface = InterfaceName::from_static_str_unchecked(ITEM_INTERFACE);
        let values = properties.get_all(interface.clone()).await?;
        let kind = MenuKind::of(&get::<String>(&values, "Id").unwrap_or_default());
        let menu_path = get::<OwnedObjectPath>(&values, "Menu")
            .filter(|path| !matches!(path.as_str(), "/" | "/NO_DBUSMENU"));
        let menu: Option<zbus::Proxy<'_>> = match &menu_path {
            Some(path) => Some(
                zbus::proxy::Builder::new(&conn)
                    .destination(name.as_str())
                    .path(path.as_str())
                    .interface(MENU_INTERFACE)
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await?,
            ),
            None => None,
        };
        // LayoutUpdated and ItemsPropertiesUpdated.
        let mut menu_changes = match &menu {
            Some(menu) => Some(menu.receive_all_signals().await?),
            None => None,
        };
        let (clicks, mut clicked) = mpsc::unbounded_channel();
        let mut ibus_changes = match (kind, &menu) {
            (MenuKind::Ibus, Some(_)) => ibus_property_changes(&runtime_dir).await,
            _ => None,
        };
        // When IBus's menu is to be built again.
        let mut rebuild_at = None;
        report(Update::Tray(Some(read_tray(&values, icon_dir.as_deref()))));
        let mut item_changed = false;
        // `Some(true)`: the menu is to be read after AboutToShow, as menus that fill
        // themselves when shown need. Not after a change of the menu, which AboutToShow
        // may cause.
        let mut menu_changed = Some(true);
        loop {
            if std::mem::take(&mut item_changed) {
                let values = properties.get_all(interface.clone()).await?;
                report(Update::Tray(Some(read_tray(&values, icon_dir.as_deref()))));
            }
            if let (Some(menu), Some(show)) = (&menu, menu_changed.take()) {
                if show {
                    let _: zbus::Result<bool> = menu.call("AboutToShow", &(0i32,)).await;
                }
                let layout: zbus::Result<(u32, Layout)> = menu
                    .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
                    .await;
                // A menu that cannot be read does not take the item's icon with it.
                match layout {
                    Ok((_, layout)) => {
                        debug!(name, layout = ?layout_summary(&layout), "the tray item's menu");
                        report(Update::Menu(Some(HostedMenu {
                            properties: menu_properties(&layout, kind),
                            clicks: clicks.clone(),
                        })));
                    }
                    Err(e) => {
                        debug!(name, "cannot read the tray item's menu: {e}");
                        report(Update::Menu(None));
                    }
                }
            }
            tokio::select! {
                change = changes.next() => {
                    if change.is_none() { break }
                    // What the input method does changed, and with it what its menu has.
                    item_changed = true;
                    menu_changed = Some(true);
                }
                change = async {
                    match &mut menu_changes {
                        Some(changes) => changes.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if change.is_none() { break }
                    // One read for a burst of changes.
                    if let Some(changes) = &mut menu_changes {
                        while let Some(Some(_)) = changes.next().now_or_never() {}
                    }
                    menu_changed = Some(menu_changed.unwrap_or(false));
                }
                gone = owner.next() => if !matches!(gone, Some(Some(_))) { break },
                change = async {
                    match &mut ibus_changes {
                        Some((_, changes)) => changes.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if change.is_none() {
                        ibus_changes = None;
                    } else if rebuild_at.is_none() {
                        // One rebuild for a burst of changes.
                        rebuild_at = Some(tokio::time::Instant::now() + IBUS_MENU_DELAY);
                    }
                }
                () = async {
                    match rebuild_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    rebuild_at = None;
                    // IBus does not answer: no reply is expected. The menu is read when
                    // IBus says its layout changed.
                    for _ in 0..2 {
                        let _ = item.call_noreply("SecondaryActivate", &(0i32, 0i32)).await;
                    }
                }
                Some(id) = clicked.recv() => {
                    if let Some(menu) = &menu {
                        let clicked: zbus::Result<()> = menu
                            .call("Event", &(id, "clicked", Variant::from(0i32), 0u32))
                            .await;
                        if let Err(e) = clicked {
                            debug!(name, id, "a click on the tray item's menu failed: {e}");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        debug!(name, path, "stopped following a tray item: {e}");
    }
    report(Update::Tray(None));
    report(Update::Menu(None));
}

/// The `UpdateProperty` signals of IBus's engines, on the bus of the IBus that
/// `omime-ibus` runs in `runtime_dir`, with the connection they come on.
async fn ibus_property_changes(
    runtime_dir: &Path,
) -> Option<(zbus::Connection, zbus::MessageStream)> {
    let Some(address) = super::frameworks::ibus_address(runtime_dir) else {
        debug!("IBus's address is not known: its menu may show old states");
        return None;
    };
    let result: zbus::Result<_> = async {
        let conn = zbus::connection::Builder::address(address.as_str())
            .build()
            .await?;
        let changes = zbus::MessageStream::for_match_rule(
            "type='signal',interface='org.freedesktop.IBus.Engine',member='UpdateProperty'",
            &conn,
            None,
        )
        .await?;
        Ok((conn, changes))
    }
    .await;
    result
        .inspect_err(|e| debug!("cannot follow the properties of IBus's engines: {e}"))
        .ok()
}

fn get<T: TryFrom<OwnedVariant>>(values: &HashMap<String, OwnedVariant>, key: &str) -> Option<T> {
    values.get(key)?.try_clone().ok()?.try_into().ok()
}

/// The entries below `layout`.
fn children(layout: &Layout) -> Vec<Layout> {
    layout
        .2
        .iter()
        .filter_map(|child| child.try_clone().ok()?.try_into().ok())
        .collect()
}

fn is_separator(entry: &Layout) -> bool {
    get::<String>(&entry.1, "type").as_deref() == Some("separator")
}

/// The ids, labels and toggles of a menu, for the log.
fn layout_summary(layout: &Layout) -> Vec<String> {
    children(layout)
        .iter()
        .map(|entry| {
            let mut line = format!(
                "{} {}",
                entry.0,
                get::<String>(&entry.1, "label").unwrap_or_default()
            );
            if let Some(toggle) = get::<String>(&entry.1, "toggle-type") {
                let state = get::<i32>(&entry.1, "toggle-state").unwrap_or(0);
                line += &format!(" [{toggle} {state}]");
            }
            let below = layout_summary(entry);
            if !below.is_empty() {
                line += &format!(" {below:?}");
            }
            line
        })
        .collect()
}

/// The entries of an input method's menu that are the input method's.
fn menu_properties(layout: &Layout, kind: MenuKind) -> Vec<Property> {
    let entries = children(layout);
    let entries = match kind {
        MenuKind::Fcitx5 => entries.into_iter().filter(|entry| entry.0 >= 300).collect(),
        MenuKind::Ibus => entries
            .into_iter()
            .take_while(|entry| !is_separator(entry))
            .collect(),
        MenuKind::Other => entries,
    };
    let mut properties = Vec::new();
    add_properties(entries, "", 0, &mut properties);
    properties
}

/// Adds `entries`, in the entry `parent`, and the entries below them.
fn add_properties(entries: Vec<Layout>, parent: &str, depth: usize, out: &mut Vec<Property>) {
    if depth > MENU_DEPTH {
        return;
    }
    let shown: Vec<Layout> = entries
        .into_iter()
        .filter(|entry| get::<bool>(&entry.1, "visible") != Some(false))
        .collect();
    // Separators only between entries, one at a time.
    let mut last_was_separator = true;
    let mut kept: Vec<Layout> = Vec::new();
    for entry in shown {
        let separator = is_separator(&entry);
        if !(separator && last_was_separator) {
            kept.push(entry);
        }
        last_was_separator = separator;
    }
    if kept.last().is_some_and(is_separator) {
        kept.pop();
    }
    for entry in kept {
        let values = &entry.1;
        let id = entry.0.to_string();
        let below = children(&entry);
        let kind = if is_separator(&entry) {
            PropertyKind::Separator
        } else if !below.is_empty()
            || get::<String>(values, "children-display").as_deref() == Some("submenu")
        {
            PropertyKind::Menu
        } else {
            match get::<String>(values, "toggle-type").as_deref() {
                Some("checkmark") => PropertyKind::Toggle,
                Some("radio") => PropertyKind::Radio,
                _ => PropertyKind::Action,
            }
        };
        out.push(Property {
            id: id.clone(),
            parent: parent.to_owned(),
            kind,
            label: without_access_key(&get::<String>(values, "label").unwrap_or_default()),
            icon: get::<String>(values, "icon-name").filter(|icon| !icon.is_empty()),
            tooltip: None,
            checked: get::<i32>(values, "toggle-state") == Some(1),
            enabled: get::<bool>(values, "enabled").unwrap_or(true),
        });
        if kind == PropertyKind::Menu {
            add_properties(below, &id, depth + 1, out);
        }
    }
}

/// A dbusmenu label as shown: `_` marks the access key, `__` is an underscore.
fn without_access_key(label: &str) -> String {
    let mut shown = String::new();
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => shown.extend(chars.next()),
            c => shown.push(c),
        }
    }
    shown
}

/// The interesting properties of a tray item.
fn read_tray(values: &HashMap<String, OwnedVariant>, icon_dir: Option<&str>) -> HostedTray {
    let tooltip: Option<(String, Vec<Pixmap>, String, String)> = get(values, "ToolTip");
    let icon_theme_path = get::<String>(values, "IconThemePath")
        .filter(|path| !path.is_empty())
        .or_else(|| icon_dir.map(str::to_owned))
        .unwrap_or_default();
    HostedTray {
        icon_name: get(values, "IconName").unwrap_or_default(),
        icon_pixmap: get(values, "IconPixmap").unwrap_or_default(),
        icon_theme_path,
        title: tooltip.map(|(_, _, title, _)| title).unwrap_or_default(),
        label: get(values, "XAyatanaLabel").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::Variant as Value;

    fn owned(value: Value<'_>) -> OwnedVariant {
        OwnedVariant::try_from(value).expect("no file descriptors")
    }

    #[test]
    fn tray_items_are_read() {
        let pixmap: Vec<Pixmap> = vec![(16, 16, vec![0; 16 * 16 * 4])];
        let values = HashMap::from([
            ("IconName".to_owned(), owned(Value::from("fcitx-pinyin"))),
            ("IconPixmap".to_owned(), owned(Value::from(pixmap.clone()))),
            ("IconThemePath".to_owned(), owned(Value::from(""))),
            (
                "ToolTip".to_owned(),
                owned(Value::from((
                    "fcitx-pinyin".to_owned(),
                    Vec::<Pixmap>::new(),
                    "拼音".to_owned(),
                    String::new(),
                ))),
            ),
            ("XAyatanaLabel".to_owned(), owned(Value::from("拼"))),
        ]);
        let tray = read_tray(&values, Some("/usr/share/icons"));
        assert_eq!(
            tray,
            HostedTray {
                icon_name: "fcitx-pinyin".to_owned(),
                icon_pixmap: pixmap,
                // The item names no path: the input method's own icons are used.
                icon_theme_path: "/usr/share/icons".to_owned(),
                title: "拼音".to_owned(),
                label: "拼".to_owned(),
            }
        );
        assert!(tray.has_icon());
        assert_eq!(read_tray(&HashMap::new(), None), HostedTray::default());
    }

    /// A menu entry: `extra` are more properties, as (name, value).
    fn entry(id: i32, label: &str, extra: &[(&str, Value<'_>)], below: Vec<Layout>) -> Layout {
        let mut values = HashMap::from([("label".to_owned(), owned(Value::from(label)))]);
        for (name, value) in extra {
            values.insert((*name).to_owned(), owned(value.clone()));
        }
        let below = below
            .into_iter()
            .map(|child| owned(Value::from(zbus::Structure::from(child))))
            .collect();
        (id, values, below)
    }

    fn separator(id: i32) -> Layout {
        entry(id, "", &[("type", Value::from("separator"))], Vec::new())
    }

    fn labels(properties: &[Property]) -> Vec<(&str, &str, &str)> {
        properties
            .iter()
            .map(|p| (p.id.as_str(), p.parent.as_str(), p.label.as_str()))
            .collect()
    }

    #[test]
    fn fcitx5_menus_give_the_status_actions() {
        // What Fcitx 5.1 shows with Pinyin.
        let menu = entry(
            0,
            "",
            &[],
            vec![
                entry(100, "Keyboard - English (US)", &[], Vec::new()),
                entry(101, "Pinyin", &[], Vec::new()),
                separator(2),
                entry(304, "Simplified Chinese", &[], Vec::new()),
                entry(301, "Full width punctuation", &[], Vec::new()),
                entry(
                    302,
                    "Prediction",
                    &[
                        ("toggle-type", Value::from("checkmark")),
                        ("toggle-state", Value::from(1i32)),
                    ],
                    Vec::new(),
                ),
                separator(3),
                entry(5, "Restart", &[], Vec::new()),
                entry(4, "Input Method Settings", &[], Vec::new()),
                entry(6, "Exit", &[], Vec::new()),
            ],
        );
        let properties = menu_properties(&menu, MenuKind::of("Fcitx"));
        assert_eq!(
            labels(&properties),
            [
                ("304", "", "Simplified Chinese"),
                ("301", "", "Full width punctuation"),
                ("302", "", "Prediction"),
            ]
        );
        assert_eq!(properties[0].kind, PropertyKind::Action);
        assert_eq!(properties[2].kind, PropertyKind::Toggle);
        assert!(properties[2].checked);
    }

    #[test]
    fn ibus_menus_give_the_engines_properties() {
        // What IBus 1.5 shows with libpinyin.
        let menu = entry(
            0,
            "",
            &[],
            vec![
                entry(13, "_Chinese", &[], Vec::new()),
                entry(
                    14,
                    "Width",
                    &[("children-display", Value::from("submenu"))],
                    vec![
                        entry(
                            30,
                            "Full",
                            &[("toggle-type", Value::from("radio"))],
                            Vec::new(),
                        ),
                        separator(31),
                        separator(32),
                        entry(
                            33,
                            "Half",
                            &[
                                ("toggle-type", Value::from("radio")),
                                ("enabled", Value::from(false)),
                            ],
                            Vec::new(),
                        ),
                        separator(34),
                    ],
                ),
                entry(15, "Hidden", &[("visible", Value::from(false))], Vec::new()),
                entry(17, "Preferences", &[], Vec::new()),
                separator(18),
                entry(19, "English - English (US)", &[], Vec::new()),
                separator(20),
                entry(21, "Preferences", &[], Vec::new()),
                entry(22, "Emoji Choice", &[], Vec::new()),
            ],
        );
        let properties = menu_properties(&menu, MenuKind::of("ibus-ui-gtk3"));
        assert_eq!(
            labels(&properties),
            [
                ("13", "", "Chinese"),
                ("14", "", "Width"),
                ("30", "14", "Full"),
                ("31", "14", ""),
                ("33", "14", "Half"),
                ("17", "", "Preferences"),
            ]
        );
        assert_eq!(properties[1].kind, PropertyKind::Menu);
        assert_eq!(properties[2].kind, PropertyKind::Radio);
        assert_eq!(properties[3].kind, PropertyKind::Separator);
        assert!(!properties[4].enabled);
        // A menu without the engine's properties.
        let menu = entry(
            0,
            "",
            &[],
            vec![separator(2), entry(4, "Preferences", &[], Vec::new())],
        );
        assert!(menu_properties(&menu, MenuKind::Ibus).is_empty());
        // Other input methods: the whole menu.
        assert_eq!(menu_properties(&menu, MenuKind::of("qingjian")).len(), 1);
    }
}
