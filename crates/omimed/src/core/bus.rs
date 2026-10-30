//! Private session buses for hosted input methods.
//!
//! A hosted input method must not reach the user's session bus. Fcitx5, for one, would
//! take `org.fcitx.Fcitx5` from omimed's Fcitx5 frontend (or from a Fcitx5 the user
//! runs elsewhere) and answer applications itself. So each hosted input method gets a
//! bus of its own, a `dbus-daemon` without service activation that lives as long as
//! the input method.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long the bus may take to come up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Starts a bus listening in `dir`. Returns the bus process, which stops when dropped,
/// and its address.
pub async fn start(dir: &Path) -> anyhow::Result<(Child, String)> {
    let config = dir.join("bus.conf");
    std::fs::write(&config, config_text(&dir.join("bus")))
        .with_context(|| format!("cannot write {}", config.display()))?;
    let mut command = Command::new("dbus-daemon");
    command
        .args(["--nofork", "--nopidfile", "--print-address=1"])
        .arg(format!("--config-file={}", config.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    super::engines::exit_with_omimed(&mut command);
    let mut child = command.spawn().context("cannot start dbus-daemon")?;
    let stdout = child.stdout.take().context("dbus-daemon has no output")?;
    let mut address = String::new();
    tokio::time::timeout(
        STARTUP_TIMEOUT,
        BufReader::new(stdout).read_line(&mut address),
    )
    .await
    .context("dbus-daemon did not start in time")?
    .context("cannot read the address of dbus-daemon")?;
    let address = address.trim();
    if address.is_empty() {
        bail!("dbus-daemon exited without a bus");
    }
    Ok((child, address.to_owned()))
}

/// A session bus on the socket `socket` for programs of this user only, without
/// service activation, so that nothing on it can launch the user's desktop services.
fn config_text(socket: &Path) -> String {
    let socket = socket
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={socket}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_path_is_escaped() {
        let text = config_text(Path::new("/run/user/1000/oh-my-ime/a&b/bus"));
        assert!(text.contains("<listen>unix:path=/run/user/1000/oh-my-ime/a&amp;b/bus</listen>"));
        assert!(!text.contains("servicedir"));
    }
}
