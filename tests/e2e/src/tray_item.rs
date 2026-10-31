//! A tray item (`org.kde.StatusNotifierItem`) for test input methods, as Fcitx 5 and
//! IBus publish one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use zbus::OwnedObjectPath;
use zbus::object_server::SignalEmitter;

/// An image of `IconPixmap`: width, height, ARGB32 data.
pub type Pixmap = (i32, i32, Vec<u8>);

/// What the item shows.
#[derive(Debug, Clone, Default)]
pub struct TrayItem {
    pub icon: String,
    pub title: String,
    pub label: String,
}

impl TrayItem {
    /// Parses `ICON|TITLE|LABEL`.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let mut parts = spec.splitn(3, '|');
        let mut next = || parts.next().unwrap_or_default().to_owned();
        Ok(Self {
            icon: next(),
            title: next(),
            label: next(),
        })
    }
}

struct Item(Arc<Mutex<TrayItem>>);

impl Item {
    fn get(&self) -> TrayItem {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {}

    #[zbus(property, name = "Id")]
    fn id(&self) -> String {
        "omime-test".to_owned()
    }

    #[zbus(property, name = "Title")]
    fn title(&self) -> String {
        "Test input method".to_owned()
    }

    #[zbus(property, name = "Status")]
    fn status(&self) -> String {
        "Active".to_owned()
    }

    #[zbus(property, name = "IconName")]
    fn icon_name(&self) -> String {
        self.get().icon
    }

    #[zbus(property, name = "IconPixmap")]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property, name = "ToolTip")]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let item = self.get();
        (item.icon, Vec::new(), item.title, String::new())
    }

    #[zbus(property, name = "XAyatanaLabel")]
    fn x_ayatana_label(&self) -> String {
        self.get().label
    }

    #[zbus(property, name = "Menu")]
    fn menu(&self) -> OwnedObjectPath {
        zbus::ObjectPath::from_static_str_unchecked("/NO_DBUSMENU").into()
    }

    #[zbus(signal, name = "NewIcon")]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The published item: [`Self::set`] changes what it shows.
#[derive(Debug, Clone)]
pub struct PublishedItem {
    item: Arc<Mutex<TrayItem>>,
    changed: tokio::sync::mpsc::UnboundedSender<()>,
}

impl PublishedItem {
    pub fn set(&self, item: TrayItem) {
        *self.item.lock().unwrap_or_else(|p| p.into_inner()) = item;
        let _ = self.changed.send(());
    }
}

/// Publishes `item` on the session bus from a thread of its own, and registers it with
/// the watcher by its unique name, as Fcitx 5 does, retrying until a watcher answers.
pub fn publish(item: TrayItem) -> anyhow::Result<PublishedItem> {
    let shared = Arc::new(Mutex::new(item));
    let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let published = PublishedItem {
        item: shared.clone(),
        changed,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start a runtime for the tray item")?;
    std::thread::spawn(move || {
        runtime.block_on(async move {
            let Ok(conn) = zbus::connection::Builder::session()
                .serve_at("/StatusNotifierItem", Item(shared))
                .build()
                .await
            else {
                eprintln!("the tray item has no session bus");
                return;
            };
            let name = conn
                .unique_name()
                .map(|n| n.to_string())
                .unwrap_or_default();
            loop {
                let result = conn
                    .call_method(
                        Some("org.kde.StatusNotifierWatcher"),
                        "/StatusNotifierWatcher",
                        Some("org.kde.StatusNotifierWatcher"),
                        "RegisterStatusNotifierItem",
                        &(name.as_str(),),
                    )
                    .await;
                if result.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let Ok(emitter) = SignalEmitter::new(&conn, "/StatusNotifierItem") else {
                return;
            };
            while changes.recv().await.is_some() {
                let _ = Item::new_icon(&emitter).await;
            }
            std::future::pending::<()>().await;
        });
    });
    Ok(published)
}
