//! `omime-ibus` — runs IBus as an engine of oh-my-ime.
//!
//! IBus is several processes: `ibus-daemon`, which runs the engines, a panel that draws
//! candidates, and a Wayland input method client. Since IBus 1.5.30 the GTK panel can be
//! the input method itself (`ibus-ui-gtk3 --enable-wayland-im`), which makes its
//! candidate window an input popup, placed at the text cursor like any engine window.
//! The panel exits at once when no daemon answers, and the daemon starts a panel of its
//! own unless told not to, so no single command runs IBus as a hosted input method. This
//! program starts `ibus-daemon --panel disable`, waits for its address file, starts the
//! panel as the input method, and stops both when either exits or omimed stops it.
//!
//! ```text
//! omime-ibus [IBUS-DAEMON ARGUMENT]...
//! ```
//!
//! The address file is the one IBus tools in the session look for, named after the
//! session's display (`~/.config/ibus/bus/<machine id>-unix-<WAYLAND_DISPLAY>`), so
//! `ibus engine`, `ibus-setup` and friends reach the hosted daemon unchanged;
//! `IBUS_ADDRESS_FILE` overrides it. `OMIME_IBUS_DAEMON` and `OMIME_IBUS_PANEL` name the
//! programs when they are not `ibus-daemon` on `$PATH` and `ibus-ui-gtk3` in the
//! matching `libexec`.

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};

/// How long ibus-daemon may take to write its address.
const DAEMON_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the processes get after SIGTERM, together. omimed sends SIGKILL two seconds
/// after its own SIGTERM.
const GRACE: Duration = Duration::from_millis(1500);

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("omime-ibus: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<ExitCode> {
    let daemon_program = program("OMIME_IBUS_DAEMON", "ibus-daemon")?;
    let panel_program = panel_program(&daemon_program)?;
    let address_file = address_file()?;
    // A stale file would send the panel to a daemon that is gone.
    let _ = std::fs::remove_file(&address_file);

    // No panel of the daemon's own: the one started below is the panel. The emoji
    // extension is a window that needs the keyboard, which an engine window cannot get.
    let mut args: Vec<String> = ["--panel", "disable", "--emoji-extension", "disable"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(std::env::args().skip(1));
    let mut daemon = spawn(&daemon_program, &args, &address_file)?;
    eprintln!(
        "omime-ibus: started {} (address file {})",
        daemon_program.display(),
        address_file.display()
    );
    let deadline = Instant::now() + DAEMON_TIMEOUT;
    loop {
        if std::fs::read_to_string(&address_file).is_ok_and(|text| text.contains("IBUS_ADDRESS=")) {
            break;
        }
        if let Some(status) = daemon.try_wait()? {
            bail!("ibus-daemon exited before it wrote its address: {status}");
        }
        if Instant::now() > deadline {
            stop(&mut daemon).await;
            bail!(
                "ibus-daemon did not write {} in time",
                address_file.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut panel = spawn(
        &panel_program,
        &["--enable-wayland-im".to_owned()],
        &address_file,
    )?;
    eprintln!(
        "omime-ibus: started {} as the input method",
        panel_program.display()
    );

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let first = tokio::select! {
        status = daemon.wait() => Some(("ibus-daemon", status?)),
        status = panel.wait() => Some(("ibus-ui-gtk3", status?)),
        _ = terminate.recv() => None,
        _ = interrupt.recv() => None,
    };
    tokio::join!(stop(&mut panel), stop(&mut daemon));
    Ok(match first {
        None => ExitCode::SUCCESS,
        Some((name, status)) => {
            eprintln!("omime-ibus: {name} exited: {status}");
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    })
}

/// Starts `program`, which exits with this process.
#[allow(unsafe_code)]
fn spawn(program: &Path, args: &[String], address_file: &Path) -> anyhow::Result<Child> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env("IBUS_ADDRESS_FILE", address_file)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let parent = rustix::process::getpid();
    // SAFETY: the closure runs in the child between fork and exec. It makes three
    // system calls and allocates nothing (errors are plain error numbers).
    unsafe {
        command.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
            // This process may have died before the child asked to follow it.
            if rustix::process::getppid() != Some(parent) {
                return Err(rustix::io::Errno::SRCH.into());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .with_context(|| format!("cannot start {}", program.display()))
}

/// Stops a process politely, if it still runs: SIGTERM, and SIGKILL after [`GRACE`].
async fn stop(child: &mut Child) {
    let Some(pid) = child.id() else { return };
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    if let Ok(pid) = i32::try_from(pid)
        && let Some(pid) = rustix::process::Pid::from_raw(pid)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

/// The program `$env` names, or `name` on `$PATH`.
fn program(env: &str, name: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        return Ok(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("{name} is not on $PATH; is IBus installed?"))
}

/// IBus's GTK panel, `ibus-ui-gtk3`. It is not on `$PATH`: it lives in `libexec` next to
/// the daemon's `bin` (Fedora, nixpkgs) or in `lib/ibus` (Debian).
fn panel_program(daemon: &Path) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("OMIME_IBUS_PANEL") {
        return Ok(PathBuf::from(path));
    }
    let mut candidates = Vec::new();
    if let Some(prefix) = std::fs::canonicalize(daemon)
        .ok()
        .and_then(|daemon| Some(daemon.parent()?.parent()?.to_path_buf()))
    {
        candidates.push(prefix.join("libexec/ibus-ui-gtk3"));
        candidates.push(prefix.join("lib/ibus/ibus-ui-gtk3"));
    }
    candidates.extend(
        [
            "/usr/libexec/ibus-ui-gtk3",
            "/usr/lib/ibus/ibus-ui-gtk3",
            "/usr/lib64/ibus/ibus-ui-gtk3",
        ]
        .map(PathBuf::from),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .context("cannot find ibus-ui-gtk3, IBus's panel; set $OMIME_IBUS_PANEL to it")
}

/// Where IBus tools in the session look for the daemon's address: the file IBus names
/// after the session's display (`ibus_get_socket_path`). The hosted processes see
/// omimed's bridge socket in `WAYLAND_DISPLAY`; omimed passes the session's display in
/// `OMIME_HOST_WAYLAND_DISPLAY`.
fn address_file() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("IBUS_ADDRESS_FILE") {
        return Ok(PathBuf::from(path));
    }
    let wayland = std::env::var("OMIME_HOST_WAYLAND_DISPLAY")
        .or_else(|_| std::env::var("WAYLAND_DISPLAY"))
        .ok();
    let (host, number) = match wayland {
        Some(display) => ("unix".to_owned(), display),
        None => {
            // `host:number.screen`, with an empty host for the local X server.
            let display = std::env::var("DISPLAY").unwrap_or_default();
            let (host, rest) = display.split_once(':').unwrap_or((&display, "0"));
            let number = rest.split('.').next().unwrap_or("0");
            let host = if host.is_empty() { "unix" } else { host };
            (host.to_owned(), number.to_owned())
        }
    };
    let machine = ["/var/lib/dbus/machine-id", "/etc/machine-id"]
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map_or_else(|| "machine-id".to_owned(), |id| id.trim().to_owned());
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("neither $XDG_CONFIG_HOME nor $HOME is set")?;
    Ok(config
        .join("ibus/bus")
        .join(format!("{machine}-{host}-{number}")))
}
