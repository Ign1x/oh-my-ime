//! Setup and daemon startup across supported, unsupported and delayed compositors.

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use omime_e2e::harness::{TestEnv, echo_engine, program_dir, wait_until, wait_until_within};

const DISPLAY: &str = "weston-unsupported";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn supported_setup_needs_no_library_path() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let output = env
        .command(program_dir().join("omime"))
        .env_remove("LD_LIBRARY_PATH")
        .args(["setup", "--dry-run"])
        .output()?;
    let message = text(&output);
    assert!(output.status.success(), "{message}");
    assert!(!message.contains("cannot check"), "{message}");
    assert!(!message.contains("would be refused"), "{message}");
    assert!(message.contains("Would Create"), "{message}");
    Ok(())
}

struct Resume(rustix::process::Pid);

impl Drop for Resume {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn a_slow_compositor_preserves_the_startup_engine_and_connection() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[echo_engine()])?;
    let ipc = env.command("printenv").arg("SWAYSOCK").output()?;
    let ipc = String::from_utf8(ipc.stdout)?;
    let pid = Path::new(ipc.trim())
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit('.').next())
        .context("sway pid in IPC socket name")?
        .parse()?;
    let pid = rustix::process::Pid::from_raw(pid).context("valid sway pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
    let resume = Resume(pid);
    // Start directly: the harness normally waits for the compositor to answer.
    let log_path = env.dir().join("slow-compositor.log");
    let _daemon = Process(
        env.command(program_dir().join("omimed"))
            .args(["--engine", "echo"])
            .env("OMIME_LOG", "debug")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log_path)?)
            .spawn()?,
    );
    wait_until("sway to be stopped", || {
        let status = fs::read_to_string(format!("/proc/{}/status", pid.as_raw_nonzero())).ok()?;
        status
            .lines()
            .any(|line| line.starts_with("State:\tT"))
            .then_some(())
    })?;
    std::thread::sleep(Duration::from_secs(3));
    drop(resume);
    env.wait_for_status("echo")
        .with_context(|| fs::read_to_string(&log_path).unwrap_or_default())?;
    let log = fs::read_to_string(&log_path)?;
    assert!(!log.contains("lost the compositor connection"), "{log}");
    assert!(!log.contains("reconnecting"), "{log}");
    assert!(!log.contains("ERROR"), "{log}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn a_first_registry_connection_loss_keeps_the_startup_engine() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[echo_engine()])?;
    let display = env.command("printenv").arg("WAYLAND_DISPLAY").output()?;
    let display = String::from_utf8(display.stdout)?;
    let socket = env.dir().join("disconnect-once");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/registry_disconnect.py");
    let _proxy = Process(
        env.command("python3")
            .arg(script)
            .arg(&socket)
            .arg(env.dir().join(display.trim()))
            .stderr(fs::File::create(env.dir().join("disconnect-once.log"))?)
            .spawn()?,
    );
    wait_until("the proxy socket", || socket.exists().then_some(()))?;
    let log_path = env.dir().join("registry-disconnect.log");
    let _daemon = Process(
        env.command(program_dir().join("omimed"))
            .arg("--display")
            .arg(&socket)
            .args(["--engine", "echo"])
            .env("OMIME_LOG", "debug")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log_path)?)
            .spawn()?,
    );
    env.wait_for_status("echo")
        .with_context(|| fs::read_to_string(&log_path).unwrap_or_default())?;
    let log = fs::read_to_string(log_path)?;
    assert_eq!(
        log.matches("lost the compositor connection").count(),
        1,
        "{log}"
    );
    assert_eq!(log.matches("reconnecting").count(), 1, "{log}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; needs the E2E shell"]
fn seat_names_received_with_initial_sync_are_preserved() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let socket = env.dir().join("scripted-compositor");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/seat_metadata.py");
    let _compositor = Process(
        env.command("python3")
            .arg(script)
            .arg(&socket)
            .stderr(fs::File::create(env.dir().join("scripted-compositor.log"))?)
            .spawn()?,
    );
    wait_until("the scripted compositor socket", || {
        socket.exists().then_some(())
    })?;
    let log_path = env.dir().join("seat-metadata.log");
    let _daemon = Process(
        env.command("sh")
            .args(["-c", "sleep 0.25; exec \"$@\"", "delayed-omimed"])
            .arg(program_dir().join("omimed"))
            .arg("--display")
            .arg(&socket)
            .env("OMIME_LOG", "debug")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log_path)?)
            .spawn()?,
    );
    // Doctor probes when the daemon is absent; only omimed may use this fixture's
    // sole connection. Status waits for D-Bus readiness without touching Wayland.
    env.wait_for_status("direct input")
        .with_context(|| fs::read_to_string(&log_path).unwrap_or_default())?;
    let diagnosis = wait_until("the daemon's seat diagnosis", || {
        let output = env
            .command(program_dir().join("omime"))
            .env("WAYLAND_DISPLAY", &socket)
            .arg("doctor")
            .output()
            .ok()?;
        let message = text(&output);
        message
            .contains("oh-my-ime is the input method")
            .then_some(message)
    })
    .with_context(|| fs::read_to_string(&log_path).unwrap_or_default())?;
    assert!(diagnosis.contains("seat seat0:"), "{diagnosis}");
    assert!(!diagnosis.contains("seat#3"), "{diagnosis}");
    Ok(())
}

fn weston(env: &TestEnv) -> anyhow::Result<Process> {
    let log = fs::File::create(env.dir().join("weston.log"))?;
    let process = Process(
        env.command("weston")
            .args([
                "--backend=headless",
                "--renderer=pixman",
                "--shell=kiosk-shell.so",
                "--no-config",
                "--idle-time=0",
                "--socket",
                DISPLAY,
            ])
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .context("weston must be installed in the E2E shell")?,
    );
    wait_until("Weston's socket", || {
        env.dir().join(DISPLAY).exists().then_some(())
    })
    .with_context(|| fs::read_to_string(env.dir().join("weston.log")).unwrap_or_default())?;
    Ok(process)
}

fn omime(env: &TestEnv, args: &[&str]) -> anyhow::Result<Output> {
    Ok(env
        .command(program_dir().join("omime"))
        .env("WAYLAND_DISPLAY", DISPLAY)
        .args(args)
        .output()?)
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[allow(unsafe_code)]
fn inherit_socket(command: &mut Command, socket: std::os::unix::net::UnixStream) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    command.env("WAYLAND_SOCKET", socket.as_raw_fd().to_string());
    // SAFETY: the closure only calls async-signal-safe fcntl; it owns the socket,
    // which remains open across exec and is separate from the child's stdio.
    unsafe {
        command.pre_exec(move || {
            let flags = rustix::io::fcntl_getfd(&socket)?;
            rustix::io::fcntl_setfd(&socket, flags & !rustix::io::FdFlags::CLOEXEC)?;
            Ok(())
        });
    }
}

fn snapshot(path: &Path) -> anyhow::Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                files.extend(snapshot(&path)?);
            } else {
                files.push((path.clone(), fs::read(path)?));
            }
        }
    }
    files.sort();
    Ok(files)
}

struct Framework(Arc<AtomicBool>);

#[zbus::interface(name = "org.fcitx.Fcitx.Controller1")]
impl Framework {
    fn exit(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn unsupported_setup_preserves_files_and_the_framework_owner() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let _weston = weston(&env)?;
    let exited = Arc::new(AtomicBool::new(false));
    let observed = exited.clone();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let address = format!("unix:path={}", env.dir().join("bus").display());
            let framework = zbus::connection::Builder::address(address.as_str())
                .name("org.fcitx.Fcitx5")
                .serve_at("/controller", Framework(exited))
                .build()
                .await?;
            let owner = framework
                .unique_name()
                .context("framework owner")?
                .to_string();
            let exits = observed.clone();
            let _env = tokio::task::spawn_blocking(move || -> anyhow::Result<TestEnv> {
                let config = snapshot(&env.dir().join("config"))?;
                let state = snapshot(&env.dir().join("state"))?;
                for args in [vec!["setup", "--start", "--take-over"], vec!["setup"]] {
                    let output = omime(&env, &args)?;
                    assert!(!exits.load(Ordering::SeqCst), "the framework received Exit");
                    assert!(!output.status.success());
                    let message = text(&output);
                    assert!(message.contains("zwp_input_method_manager_v2"), "{message}");
                    assert!(
                        message.contains("zwp_virtual_keyboard_manager_v1"),
                        "{message}"
                    );
                    assert!(message.contains("KDE Plasma and GNOME"), "{message}");
                    assert!(message.contains("Hyprland session"), "{message}");
                }
                let preview = omime(&env, &["setup", "--start", "--take-over", "--dry-run"])?;
                assert!(preview.status.success(), "{}", text(&preview));
                let preview = text(&preview);
                assert!(
                    preview.contains("applying setup would be refused"),
                    "{preview}"
                );
                assert!(
                    preview.find("not supported").unwrap() < preview.find("Would Create").unwrap()
                );
                let doctor = omime(&env, &["doctor"])?;
                assert!(!doctor.status.success());
                let diagnosis = text(&doctor);
                assert!(diagnosis.contains("KDE Plasma and GNOME"), "{diagnosis}");
                assert!(!diagnosis.contains("omimed is not running"), "{diagnosis}");
                assert_eq!(snapshot(&env.dir().join("config"))?, config);
                assert_eq!(snapshot(&env.dir().join("state"))?, state);
                assert!(!env.dir().join("state/oh-my-ime/setup.lock").exists());
                assert!(!env.dir().join("state/oh-my-ime/omimed.log").exists());
                // A text console can prepare the next login, and undo is always available.
                let offline = env
                    .command(program_dir().join("omime"))
                    .env_remove("WAYLAND_DISPLAY")
                    .args(["setup"])
                    .output()?;
                assert!(offline.status.success(), "{}", text(&offline));
                assert!(env.dir().join("state/oh-my-ime/setup.toml").exists());
                let undo = omime(&env, &["setup", "--undo"])?;
                assert!(undo.status.success(), "{}", text(&undo));
                assert_eq!(snapshot(&env.dir().join("config"))?, config);
                Ok(env)
            })
            .await??;
            assert!(
                !observed.load(Ordering::SeqCst),
                "the framework received Exit"
            );
            let bus = zbus::fdo::DBusProxy::new(&framework).await?;
            assert!(
                !bus.name_has_owner("io.github.ign1x.OhMyIme".try_into()?)
                    .await?
            );
            assert_eq!(
                bus.get_name_owner("org.fcitx.Fcitx5".try_into()?)
                    .await?
                    .as_str(),
                owner
            );
            Ok::<_, anyhow::Error>(())
        })
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn inherited_wayland_socket_refuses_before_taking_over_a_different_display() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let _weston = weston(&env)?;
    let exited = Arc::new(AtomicBool::new(false));
    let observed = exited.clone();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let address = format!("unix:path={}", env.dir().join("bus").display());
            let framework = zbus::connection::Builder::address(address.as_str())
                .name("org.fcitx.Fcitx5")
                .serve_at("/controller", Framework(exited))
                .build()
                .await?;
            let exits = observed.clone();
            let _env = tokio::task::spawn_blocking(move || -> anyhow::Result<TestEnv> {
                let config = snapshot(&env.dir().join("config"))?;
                let state = snapshot(&env.dir().join("state"))?;
                for display_set in [true, false] {
                    for dry_run in [false, true] {
                        let socket =
                            std::os::unix::net::UnixStream::connect(env.dir().join(DISPLAY))?;
                        // WAYLAND_DISPLAY names sway; the inherited descriptor connects to Weston.
                        let mut command = env.command(program_dir().join("omime"));
                        inherit_socket(&mut command, socket);
                        command.args(["setup", "--start", "--take-over"]);
                        if !display_set {
                            command.env_remove("WAYLAND_DISPLAY");
                        }
                        if dry_run {
                            command.arg("--dry-run");
                        }
                        let output = command.output()?;
                        assert!(!exits.load(Ordering::SeqCst), "the framework received Exit");
                        assert_eq!(output.status.success(), dry_run, "{}", text(&output));
                        let message = text(&output);
                        assert!(message.contains("WAYLAND_SOCKET"), "{message}");
                        if dry_run {
                            assert!(
                                message.contains("applying setup would be refused"),
                                "{message}"
                            );
                        }
                    }
                }
                assert_eq!(snapshot(&env.dir().join("config"))?, config);
                assert_eq!(snapshot(&env.dir().join("state"))?, state);
                assert!(!env.dir().join("state/oh-my-ime/setup.lock").exists());
                Ok(env)
            })
            .await??;
            assert!(!observed.load(Ordering::SeqCst));
            let bus = zbus::fdo::DBusProxy::new(&framework).await?;
            assert_eq!(
                bus.get_name_owner("org.fcitx.Fcitx5".try_into()?)
                    .await?
                    .as_str(),
                framework.unique_name().context("framework owner")?.as_str()
            );
            Ok::<_, anyhow::Error>(())
        })
}

fn exit_unavailable(env: &TestEnv, mut command: Command) -> anyhow::Result<()> {
    let log_path = env.dir().join("unsupported.log");
    let mut daemon = Process(
        command
            .stderr(fs::File::create(&log_path)?)
            .stdout(Stdio::null())
            .spawn()?,
    );
    wait_unavailable(&mut daemon, &log_path)
}

fn wait_unavailable(daemon: &mut Process, log_path: &Path) -> anyhow::Result<()> {
    let status = wait_until_within(
        Duration::from_secs(5),
        "omimed to exit without retrying",
        || daemon.0.try_wait().ok().flatten(),
    )?;
    let log = fs::read_to_string(log_path)?;
    assert_eq!(status.code(), Some(69), "{log}");
    assert_eq!(
        log.lines().filter(|line| line.contains("ERROR")).count(),
        1,
        "{log}"
    );
    assert!(log.contains("zwp_input_method_manager_v2"), "{log}");
    assert!(!log.contains("reconnecting"), "{log}");
    assert!(!log.contains("lost the compositor connection"), "{log}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn a_delayed_unsupported_compositor_interrupts_stalled_bus_authentication() -> anyhow::Result<()> {
    use std::io::Read as _;

    let env = TestEnv::prepare(&[])?;
    let weston = weston(&env)?;
    for enabled in [false, true] {
        fs::write(
            env.dir().join("config/oh-my-ime/config.toml"),
            format!("[notifications]\nenabled = {enabled}\n"),
        )?;
        let path = env.dir().join(format!("late-unsupported-bus-{enabled}"));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let pid = rustix::process::Pid::from_child(&weston.0);
        rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
        let resume = Resume(pid);
        let log_path = env.dir().join(format!("late-unsupported-{enabled}.log"));
        let started = std::time::Instant::now();
        let mut daemon = Process(
            env.command(program_dir().join("omimed"))
                .env("WAYLAND_DISPLAY", DISPLAY)
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    format!("unix:path={}", path.display()),
                )
                .stderr(fs::File::create(&log_path)?)
                .stdout(Stdio::null())
                .spawn()?,
        );
        let mut authentication = wait_until("the daemon's bus authentication", || {
            listener.accept().ok().map(|(socket, _)| socket)
        })?;
        authentication.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut bytes = [0; 128];
        assert!(
            authentication.read(&mut bytes)? > 0,
            "no authentication attempt"
        );
        // Hold authentication open without replying, past the registry's early wait.
        std::thread::sleep(Duration::from_secs(3).saturating_sub(started.elapsed()));
        assert!(daemon.0.try_wait()?.is_none());
        drop(resume);
        let resumed = std::time::Instant::now();
        wait_unavailable(&mut daemon, &log_path)?;
        assert!(resumed.elapsed() < Duration::from_secs(4));
        authentication
            .read_to_end(&mut Vec::new())
            .context("startup connection was not closed")?;
        if enabled {
            let (mut notification, _) = listener.accept()?;
            notification.set_read_timeout(Some(Duration::from_secs(1)))?;
            assert!(
                notification.read(&mut bytes)? > 0,
                "no notification attempt"
            );
        } else {
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn unsupported_daemon_exits_once_and_respects_notifications() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let _weston = weston(&env)?;
    let notifications = env.notifications()?;
    for enabled in [true, false] {
        fs::write(
            env.dir().join("config/oh-my-ime/config.toml"),
            format!(
                "[tray]\nenabled = false\n[panel]\nenabled = false\n[notifications]\nenabled = {enabled}\n"
            ),
        )?;
        let mut command = env.command(program_dir().join("omimed"));
        // Also exercise --display rather than inheriting the unsupported socket.
        command.args(["--display", DISPLAY]);
        exit_unavailable(&env, command)?;
        if enabled {
            notifications.wait_for("the unsupported-desktop notification", |line| {
                line.contains("This desktop is not supported") && line.contains("omimed log")
            })?;
        }
        assert_eq!(
            notifications
                .lines()
                .iter()
                .filter(|line| line.starts_with("notify "))
                .count(),
            1
        );
    }
    fs::write(
        env.dir().join("config/oh-my-ime/config.toml"),
        "[tray]\nenabled = false\n[panel]\nenabled = false\n",
    )?;
    let mut command = env.command(program_dir().join("omimed"));
    command.env("WAYLAND_DISPLAY", DISPLAY).env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", env.dir().join("missing-bus").display()),
    );
    exit_unavailable(&env, command)?;
    Ok(())
}

struct HangingNotifications(Arc<AtomicBool>);

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl HangingNotifications {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        _app_name: &str,
        _replaces_id: u32,
        _app_icon: &str,
        _summary: &str,
        _body: &str,
        _actions: Vec<String>,
        _hints: std::collections::HashMap<String, zbus::OwnedVariant>,
        _expire_timeout: i32,
    ) -> u32 {
        self.0.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn unsupported_daemon_does_not_wait_for_a_stuck_notification_server() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[])?;
    let _weston = weston(&env)?;
    let notified = Arc::new(AtomicBool::new(false));
    let observed = notified.clone();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let address = format!("unix:path={}", env.dir().join("bus").display());
            let _server = zbus::connection::Builder::address(address.as_str())
                .name("org.freedesktop.Notifications")
                .serve_at(
                    "/org/freedesktop/Notifications",
                    HangingNotifications(notified),
                )
                .build()
                .await?;
            let _env = tokio::task::spawn_blocking(move || -> anyhow::Result<TestEnv> {
                let mut command = env.command(program_dir().join("omimed"));
                command.env("WAYLAND_DISPLAY", DISPLAY);
                exit_unavailable(&env, command)?;
                Ok(env)
            })
            .await??;
            assert!(
                observed.load(Ordering::SeqCst),
                "omimed did not try to notify"
            );
            Ok::<_, anyhow::Error>(())
        })
}

#[test]
#[ignore = "end-to-end test; needs headless Weston and the E2E shell"]
fn unsupported_daemon_does_not_wait_for_bus_authentication() -> anyhow::Result<()> {
    use std::io::Read as _;

    let env = TestEnv::prepare(&[])?;
    let _weston = weston(&env)?;
    for enabled in [true, false] {
        fs::write(
            env.dir().join("config/oh-my-ime/config.toml"),
            format!("[notifications]\nenabled = {enabled}\n"),
        )?;
        let path = env.dir().join(format!("stuck-bus-{enabled}"));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let mut command = env.command(program_dir().join("omimed"));
        command.env("WAYLAND_DISPLAY", DISPLAY).env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", path.display()),
        );
        let finished = AtomicBool::new(false);
        let accepted = std::thread::scope(|scope| -> anyhow::Result<bool> {
            let server = scope.spawn(|| -> anyhow::Result<bool> {
                let mut connections = Vec::new();
                let deadline = std::time::Instant::now() + Duration::from_secs(6);
                while !finished.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket.set_read_timeout(Some(Duration::from_secs(1)))?;
                            let mut bytes = [0; 128];
                            assert!(socket.read(&mut bytes)? > 0, "no authentication attempt");
                            // Keep every connection open without replying to authentication.
                            connections.push(socket);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => return Err(error.into()),
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(!connections.is_empty())
            });
            let started = std::time::Instant::now();
            let result = exit_unavailable(&env, command);
            finished.store(true, Ordering::SeqCst);
            let accepted = server.join().expect("bus fixture thread")?;
            result?;
            assert!(started.elapsed() < Duration::from_secs(4));
            Ok(accepted)
        })?;
        assert_eq!(accepted, enabled);
    }
    Ok(())
}
