//! Programs that come with omimed: found next to it, as installed, or else in `$PATH`.
//! The switcher (see [`crate::panel`]), and the settings, `omime-settings`, which the
//! tray menu opens.

use std::path::PathBuf;

use omime_proto::paths::find_program;
use tracing::{info, warn};

/// The settings program.
pub const SETTINGS: &str = "omime-settings";

/// The program `name` next to omimed, or else in `$PATH`.
pub fn find(name: &str) -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)))
        .and_then(find_program);
    beside.or_else(|| find_program(name))
}

/// Opens the settings on the compositor at `display` (`$WAYLAND_DISPLAY` when `None`):
/// a window of its own, which stays open when omimed stops.
pub fn open_settings(display: Option<&str>) -> Result<(), String> {
    open(display, &[])
}

/// Opens the settings on their page `page`, such as `about`.
pub fn open_settings_page(display: Option<&str>, page: &str) -> Result<(), String> {
    open(display, &[page])
}

fn open(display: Option<&str>, args: &[&str]) -> Result<(), String> {
    let program = find(SETTINGS)
        .ok_or_else(|| omime_proto::tr!("{program} is not installed", program = SETTINGS))?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(display) = display {
        command.env("WAYLAND_DISPLAY", display);
    }
    let mut child = command
        .spawn()
        .map_err(|e| omime_proto::tr!("cannot open the settings: {e}", e = e))?;
    info!(pid = child.id(), "opened the settings");
    // Collects its exit status, so that no zombie stays behind.
    let reaper = std::thread::Builder::new()
        .name("omime-settings".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(e) = reaper {
        warn!("cannot wait for the settings: {e}");
    }
    Ok(())
}
