//! `omime-portal` — the desktop's settings portal (`org.freedesktop.portal.Settings`)
//! for the end-to-end tests, as xdg-desktop-portal would be: it says which color
//! scheme the desktop prefers.
//!
//! ```text
//! omime-portal [dark|light|none]
//! ```
//!
//! It prints `ready` once it owns the name, then reads `dark`, `light` or `none`
//! from stdin, emits `SettingChanged` for it and prints `scheme <name>`.

use std::io::{BufRead as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context as _;
use zbus::object_server::SignalEmitter;
use zbus::{OwnedVariant, Variant};

const PATH: &str = "/org/freedesktop/portal/desktop";
const NAME: &str = "org.freedesktop.portal.Desktop";
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// The portal's number for a color scheme: no preference, dark, light.
fn scheme(name: &str) -> Option<u32> {
    match name {
        "none" => Some(0),
        "dark" => Some(1),
        "light" => Some(2),
        _ => None,
    }
}

struct Settings {
    scheme: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    async fn read_one(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedVariant> {
        if namespace == APPEARANCE && key == COLOR_SCHEME {
            Ok(OwnedVariant::from(self.scheme.load(Ordering::Relaxed)))
        } else {
            Err(zbus::fdo::Error::UnknownProperty(format!(
                "{namespace} {key}"
            )))
        }
    }

    #[zbus(signal)]
    async fn setting_changed(
        emitter: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: Variant<'_>,
    ) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let first = std::env::args().nth(1).unwrap_or_else(|| "none".to_owned());
    let initial = scheme(&first).with_context(|| format!("unknown color scheme `{first}`"))?;
    let current = Arc::new(AtomicU32::new(initial));
    let connection = zbus::connection::Builder::session()
        .serve_at(
            PATH,
            Settings {
                scheme: current.clone(),
            },
        )
        .name(NAME)
        .build()
        .await
        .context("cannot serve the settings portal on the session bus")?;
    say("ready");
    let (lines, mut commands) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let emitter = SignalEmitter::new(&connection, PATH)?;
    while let Some(line) = commands.recv().await {
        let name = line.trim();
        let Some(value) = scheme(name) else {
            say(&format!("unknown {name}"));
            continue;
        };
        current.store(value, Ordering::Relaxed);
        Settings::setting_changed(&emitter, APPEARANCE, COLOR_SCHEME, Variant::from(value)).await?;
        say(&format!("scheme {name}"));
    }
    // Stdin closed: keep serving until killed.
    std::future::pending::<()>().await;
    Ok(())
}
