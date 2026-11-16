//! Windows where OpenGL cannot start.
//!
//! Slint draws oh-my-ime's windows with OpenGL, which lets them be translucent and
//! have round corners. On a machine without a working OpenGL driver (a virtual machine,
//! a remote desktop, a broken driver installation) Slint finds out only when it makes
//! the window, in its event loop, and has no other renderer to turn to by then: the
//! program would end, and an engine with it, leaving the user without their input
//! method.
//!
//! A program with a window therefore calls [`restart_in_software`] when its window
//! could not start: the program starts again with Slint's software renderer. It asks
//! [`software`] how it draws, because the software renderer has no translucent
//! windows: what the window leaves transparent comes out black, so the window fills
//! its rectangle instead.
//!
//! The user's own choice in `$SLINT_BACKEND` is never overridden.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;

/// The environment variable that chooses Slint's backend and renderer.
pub const BACKEND_ENV: &str = "SLINT_BACKEND";

/// Slint's software renderer in a window of winit, in [`BACKEND_ENV`].
const SOFTWARE_BACKEND: &str = "winit-software";

/// Whether `backend`, a value of [`BACKEND_ENV`], draws in software.
fn is_software(backend: &str) -> bool {
    // As Slint reads it: `backend-renderer`, or one name for either.
    let backend = backend.to_lowercase();
    let renderer = backend.split_once('-').map_or(backend.as_str(), |(_, r)| r);
    matches!(renderer, "software" | "sw")
}

/// Whether this program draws its windows in software, where windows are opaque
/// rectangles: nothing of them can be left transparent.
pub fn software() -> bool {
    std::env::var(BACKEND_ENV).is_ok_and(|backend| is_software(&backend))
}

/// Whether to start again in software after a window could not start: only when the
/// renderer was Slint's choice. With `backend` set, the user chose, or this program
/// started again already.
fn may_restart(backend: Option<OsString>) -> bool {
    backend.is_none_or(|backend| backend.is_empty())
}

/// Starts this program again with Slint's software renderer, with the arguments it
/// has, after its window could not start with the renderer Slint chose, for `reason`.
/// Call it before the program has done anything that starting again would repeat or
/// lose.
///
/// It says on stderr that it starts again, and returns only if the program is not
/// started again: `None` when the renderer was not Slint's choice (the caller then
/// reports `reason` itself), or why the program could not be started.
pub fn restart_in_software(reason: &dyn std::fmt::Display) -> Option<std::io::Error> {
    if !may_restart(std::env::var_os(BACKEND_ENV)) {
        return None;
    }
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => return Some(e),
    };
    let mut arguments = std::env::args_os();
    let name = arguments.next().unwrap_or_else(|| program.clone().into());
    eprintln!(
        "{}: {reason}; starting again with the software renderer",
        program
            .file_name()
            .unwrap_or(program.as_os_str())
            .to_string_lossy()
    );
    Some(
        std::process::Command::new(program)
            .arg0(name)
            .args(arguments)
            .env(BACKEND_ENV, SOFTWARE_BACKEND)
            .exec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_is_known_by_the_names_slint_takes() {
        for backend in [
            "winit-software",
            "winit-sw",
            "software",
            "sw",
            "Winit-Software",
        ] {
            assert!(is_software(backend), "{backend}");
        }
        for backend in [
            "",
            "winit",
            "winit-femtovg",
            "femtovg",
            "skia",
            "winit-skia-software",
        ] {
            assert!(!is_software(backend), "{backend}");
        }
    }

    #[test]
    fn only_slints_own_choice_is_replaced() {
        assert!(may_restart(None));
        assert!(may_restart(Some(OsString::new())));
        assert!(!may_restart(Some("winit-software".into())));
        assert!(!may_restart(Some("winit-femtovg".into())));
    }
}
