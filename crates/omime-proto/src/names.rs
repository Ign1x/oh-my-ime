//! Well-known D-Bus names and object paths.
//!
//! All names live below the reverse-DNS prefix of the project's home,
//! `io.github.ign1x.OhMyIme`.

/// Well-known session bus name owned by the running daemon.
pub const DAEMON_BUS_NAME: &str = "io.github.ign1x.OhMyIme";

/// Object path of the daemon's control object on the session bus and of the host
/// object on engine connections.
pub const DAEMON_OBJECT_PATH: &str = "/io/github/ign1x/OhMyIme";

/// Object path at which an engine exports its engine object.
pub const ENGINE_OBJECT_PATH: &str = "/io/github/ign1x/OhMyIme/Engine";

/// Control interface used by the CLI and status bars.
pub const DAEMON_INTERFACE: &str = "io.github.ign1x.OhMyIme.Daemon1";

/// Interface implemented by engines.
pub const ENGINE_INTERFACE: &str = "io.github.ign1x.OhMyIme.Engine1";

/// Interface the daemon exposes to engines on their private connection.
pub const ENGINE_HOST_INTERFACE: &str = "io.github.ign1x.OhMyIme.EngineHost1";

/// Environment variable holding the D-Bus address an engine must connect to.
pub const ENV_ENGINE_ADDRESS: &str = "OMIME_ENGINE_ADDRESS";

/// Environment variable holding the id under which the engine was launched.
pub const ENV_ENGINE_ID: &str = "OMIME_ENGINE_ID";

/// Environment variable naming the real compositor's display, for windows that must
/// not go through the UI bridge (for example a settings dialog).
pub const ENV_HOST_WAYLAND_DISPLAY: &str = "OMIME_HOST_WAYLAND_DISPLAY";

/// Name of the per-user runtime directory below `$XDG_RUNTIME_DIR`.
pub const RUNTIME_DIR_NAME: &str = "oh-my-ime";
