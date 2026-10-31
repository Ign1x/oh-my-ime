//! `omime-tray-host` — the tray of a status bar, for the end-to-end tests.
//!
//! It plays `org.kde.StatusNotifierWatcher` on the session bus, follows the item that
//! registers, and prints what the item shows whenever it changes, ending each snapshot
//! with `end`:
//!
//! ```text
//! ready
//! registered <service>
//! icon <IconName>
//! pixmaps <number of IconPixmap images>
//! theme <IconThemePath>
//! tooltip <the ToolTip's title>
//! label <XAyatanaLabel>
//! menu <entry>|<entry>*|...      (* marks the checked entry)
//! end
//! ```
//!
//! It reads commands from stdin: `activate` (a click), `secondary` (a middle click) and
//! `click <menu entry>`.

use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};

use anyhow::Context as _;
use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::{OwnedVariant, Variant};

type Pixmap = (i32, i32, Vec<u8>);
type Layout = (i32, HashMap<String, OwnedVariant>, Vec<OwnedVariant>);

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

struct Watcher {
    items: mpsc::UnboundedSender<(String, String)>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: String, #[zbus(header)] header: Header<'_>) {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let (name, path) = if service.starts_with('/') {
            (sender, service)
        } else {
            (service, "/StatusNotifierItem".to_owned())
        };
        let _ = self.items.send((name, path));
    }

    fn register_status_notifier_host(&self, _service: String) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// A property of `values`.
fn get<T: TryFrom<OwnedVariant>>(values: &HashMap<String, OwnedVariant>, key: &str) -> Option<T> {
    values.get(key)?.try_clone().ok()?.try_into().ok()
}

/// The entries of the menu: (id, label, checked).
fn entries(layout: &Layout) -> Vec<(i32, String, bool)> {
    layout
        .2
        .iter()
        .filter_map(|child| {
            let child: Layout = child.try_clone().ok()?.try_into().ok()?;
            let label = get::<String>(&child.1, "label").unwrap_or_default();
            let checked = get::<i32>(&child.1, "toggle-state") == Some(1);
            Some((child.0, label, checked))
        })
        .collect()
}

struct Followed<'a> {
    item: zbus::Proxy<'a>,
    properties: zbus::fdo::PropertiesProxy<'a>,
    menu: Option<zbus::Proxy<'a>>,
}

impl Followed<'_> {
    /// Prints a snapshot of the item; returns the menu's entries.
    async fn print(&self) -> anyhow::Result<Vec<(i32, String, bool)>> {
        let interface =
            zbus::names::InterfaceName::from_static_str_unchecked("org.kde.StatusNotifierItem");
        let values = self.properties.get_all(interface).await?;
        let tooltip: Option<(String, Vec<Pixmap>, String, String)> = get(&values, "ToolTip");
        say(&format!(
            "icon {}",
            get::<String>(&values, "IconName").unwrap_or_default()
        ));
        say(&format!(
            "pixmaps {}",
            get::<Vec<Pixmap>>(&values, "IconPixmap").map_or(0, |p| p.len())
        ));
        say(&format!(
            "theme {}",
            get::<String>(&values, "IconThemePath").unwrap_or_default()
        ));
        say(&format!(
            "tooltip {}",
            tooltip.map(|t| t.2).unwrap_or_default()
        ));
        say(&format!(
            "label {}",
            get::<String>(&values, "XAyatanaLabel").unwrap_or_default()
        ));
        let mut entries_now = Vec::new();
        if let Some(menu) = &self.menu {
            let (_, layout): (u32, Layout) = menu
                .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
                .await?;
            entries_now = entries(&layout);
            let shown: Vec<String> = entries_now
                .iter()
                .map(|(_, label, checked)| format!("{label}{}", if *checked { "*" } else { "" }))
                .collect();
            say(&format!("menu {}", shown.join("|")));
        }
        say("end");
        Ok(entries_now)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (items, mut registrations) = mpsc::unbounded_channel();
    let conn = zbus::connection::Builder::session()
        .serve_at("/StatusNotifierWatcher", Watcher { items })
        .name("org.kde.StatusNotifierWatcher")
        .build()
        .await
        .context("cannot serve the watcher on the session bus")?;

    let (commands_tx, mut commands) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if commands_tx.send(line).is_err() {
                break;
            }
        }
    });
    say("ready");

    let Some((name, path)) = registrations.recv().await else {
        return Ok(());
    };
    say(&format!("registered {name}"));
    let item: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
        .destination(name.as_str())
        .path(path.as_str())
        .interface("org.kde.StatusNotifierItem")
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let properties = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(name.as_str())
        .path(path.as_str())
        .build()
        .await?;
    let menu_path: Option<zbus::OwnedObjectPath> = item.get_property("Menu").await.ok();
    let menu: Option<zbus::Proxy<'_>> = match menu_path.filter(|p| p.as_str() != "/NO_DBUSMENU") {
        Some(menu_path) => Some(
            zbus::proxy::Builder::new(&conn)
                .destination(name.as_str())
                .path(menu_path)
                .interface("com.canonical.dbusmenu")
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await?,
        ),
        None => None,
    };
    let mut item_signals = item.receive_all_signals().await?;
    let mut menu_signals = match &menu {
        Some(menu) => Some(menu.receive_all_signals().await?),
        None => None,
    };
    let followed = Followed {
        item,
        properties,
        menu,
    };
    let mut entries_now = followed.print().await?;
    loop {
        tokio::select! {
            signal = item_signals.next() => {
                if signal.is_none() { break }
                entries_now = followed.print().await?;
            }
            signal = async {
                match &mut menu_signals {
                    Some(signals) => signals.next().await,
                    None => std::future::pending().await,
                }
            } => {
                if signal.is_none() { break }
                entries_now = followed.print().await?;
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command.split_once(' ').unwrap_or((command.as_str(), "")) {
                    ("activate", _) => {
                        followed.item.call_method("Activate", &(0i32, 0i32)).await?;
                    }
                    ("secondary", _) => {
                        followed.item.call_method("SecondaryActivate", &(0i32, 0i32)).await?;
                    }
                    ("click", label) => {
                        let menu = followed.menu.as_ref().context("the item has no menu")?;
                        let (id, ..) = entries_now
                            .iter()
                            .find(|(_, l, _)| l == label)
                            .with_context(|| format!("no menu entry {label:?}"))?;
                        menu.call_method(
                            "Event",
                            &(*id, "clicked", Variant::from(0i32), 0u32),
                        )
                        .await?;
                    }
                    _ => anyhow::bail!("unknown command {command:?}"),
                }
            }
        }
    }
    Ok(())
}
