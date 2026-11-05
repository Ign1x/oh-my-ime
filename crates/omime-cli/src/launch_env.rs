//! The launch environment of applications activated through D-Bus or systemd.
//! Use the session bus explicitly: never fall back to another user's manager socket.

use omime_proto::tr;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};

const MANAGER: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const TIMEOUT: Duration = Duration::from_secs(3);

pub async fn manager_environment(
    conn: &zbus::Connection,
) -> anyhow::Result<Option<HashMap<String, String>>> {
    tokio::time::timeout(TIMEOUT, async {
        let dbus = zbus::fdo::DBusProxy::new(conn).await?;
        if !dbus.name_has_owner(MANAGER.try_into()?).await? {
            return Ok(None);
        }
        let manager = zbus::Proxy::new(conn, MANAGER, MANAGER_PATH, MANAGER_INTERFACE).await?;
        let values: Vec<String> = manager.get_property("Environment").await?;
        Ok(Some(
            values
                .into_iter()
                .filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
                .collect(),
        ))
    })
    .await
    .context(tr!("the session manager did not answer"))?
}

fn same_display(expected: &str, actual: &str, runtime: &Path) -> bool {
    let path = |value: &str| {
        let path = Path::new(value);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            runtime.join(path)
        }
    };
    path(expected) == path(actual)
}

/// Updates new application launches after the daemon has acquired its input seat.
/// Processes already running retain the environment they started with.
pub async fn apply(conn: &zbus::Connection) -> anyhow::Result<()> {
    let manager_env = manager_environment(conn).await?;
    if let (Some(expected), Some(actual)) = (
        std::env::var("WAYLAND_DISPLAY").ok(),
        manager_env
            .as_ref()
            .and_then(|env| env.get("WAYLAND_DISPLAY")),
    ) {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default();
        ensure!(
            same_display(&expected, actual, Path::new(&runtime)),
            "{}",
            tr!(
                "the user manager belongs to Wayland display {actual}; launch settings were not changed for another session",
                actual = actual
            )
        );
    }
    let values: HashMap<&str, &str> = crate::setup::ENVIRONMENT.into_iter().collect();
    tokio::time::timeout(
        TIMEOUT,
        conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "UpdateActivationEnvironment",
            &values,
        ),
    )
    .await
    .context(tr!("D-Bus did not update the launch environment in time"))??;
    if manager_env.is_some() {
        let values: Vec<String> = crate::setup::ENVIRONMENT
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        tokio::time::timeout(
            TIMEOUT,
            conn.call_method(
                Some(MANAGER),
                MANAGER_PATH,
                Some(MANAGER_INTERFACE),
                "SetEnvironment",
                &values,
            ),
        )
        .await
        .context(tr!("systemd did not update the launch environment in time"))??;
        println!(
            "{}",
            tr!("Application launch settings updated for D-Bus and the systemd user manager.")
        );
    } else {
        println!(
            "{}",
            tr!(
                "Application launch settings updated for D-Bus (no systemd user manager on this bus)."
            )
        );
    }
    println!(
        "{}",
        tr!(
            "Restart existing applications to inherit these settings. Compositor and terminal launchers may need a new login."
        )
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_compare_as_socket_paths() {
        let runtime = Path::new("/run/user/1000");
        assert!(same_display(
            "wayland-1",
            "/run/user/1000/wayland-1",
            runtime
        ));
        assert!(!same_display("wayland-1", "wayland-2", runtime));
        assert!(!same_display(
            "wayland-1",
            "/run/user/1001/wayland-1",
            runtime
        ));
    }
}
