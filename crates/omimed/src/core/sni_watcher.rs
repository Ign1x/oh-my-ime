//! A tray host for hosted input methods.
//!
//! Hosted input methods such as Fcitx 5 and IBus publish a tray item
//! (`org.kde.StatusNotifierItem`) that shows their current input method, for example
//! Fcitx 5's "双拼". They run on a private bus, where no status bar can take the item.
//! omimed plays the `org.kde.StatusNotifierWatcher` there, follows the item the input
//! method registers, and reports what it shows; omimed's own tray item shows it while
//! that engine is current.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use tracing::debug;
use zbus::OwnedVariant;
use zbus::message::Header;
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;

use super::CoreEvent;

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";

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
/// (launch `generation`), and reports its tray item as [`CoreEvent::HostedTray`].
/// `icon_dir` is where the input method's icons are when its item does not say.
pub async fn start(
    engine: String,
    generation: u64,
    address: &str,
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
        let report = move |tray: Option<HostedTray>| {
            let _ = events.send(CoreEvent::HostedTray {
                engine: engine.clone(),
                generation,
                tray,
            });
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
                        report.clone(),
                    ));
                }
                () = &mut following => following = Box::pin(std::future::pending()),
            }
        }
    });
    Ok(TrayWatcher { _conn: conn, task })
}

/// Reports the item `name` `path` now and whenever it changes, until it goes away.
async fn follow(
    conn: zbus::Connection,
    name: String,
    path: String,
    icon_dir: Option<String>,
    report: impl Fn(Option<HostedTray>),
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
        loop {
            let values = properties.get_all(interface.clone()).await?;
            report(Some(read_tray(&values, icon_dir.as_deref())));
            tokio::select! {
                change = changes.next() => if change.is_none() { break },
                gone = owner.next() => if !matches!(gone, Some(Some(_))) { break },
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        debug!(name, path, "stopped following a tray item: {e}");
    }
    report(None);
}

/// The interesting properties of a tray item.
fn read_tray(values: &HashMap<String, OwnedVariant>, icon_dir: Option<&str>) -> HostedTray {
    fn get<T: TryFrom<OwnedVariant>>(
        values: &HashMap<String, OwnedVariant>,
        key: &str,
    ) -> Option<T> {
        values.get(key)?.try_clone().ok()?.try_into().ok()
    }
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
}
