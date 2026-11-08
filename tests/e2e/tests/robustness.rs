//! End-to-end tests for failure handling: engines that are slow or break the shared
//! Wayland connection must never stop the user from typing.
//!
//! See `typing.rs` for how to run them.

use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use omime_e2e::harness::{
    TIMEOUT, TestEngine, TestEnv, dummy_engine as dummy, echo_engine, hosted_engine, program_dir,
    target_dir, wait_until, wait_until_within,
};

fn saving_engine(args: &[&str]) -> TestEngine {
    TestEngine {
        id: "saver",
        program: target_dir().join("omime-shutdown-probe"),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        env: Vec::new(),
        hosted: false,
    }
}

fn wait_for_engine_exit(pid: u32) -> anyhow::Result<()> {
    wait_until("the engine to exit and be reaped", || {
        (!Path::new(&format!("/proc/{pid}")).exists()).then_some(())
    })
}

fn assert_saved(env: &TestEnv, pid: u32) {
    let saved = std::fs::read_to_string(env.dir().join("state/engine-saved")).unwrap_or_default();
    assert!(
        saved.lines().any(|line| line == pid.to_string()),
        "engine {pid} did not finish its 50 ms save\n{}",
        env.daemon_log()
    );
}

fn enable_saver_settings(env: &TestEnv) -> anyhow::Result<()> {
    let manifest = env.dir().join("engines/saver/engine.toml");
    let mut text = std::fs::read_to_string(&manifest)?;
    text += "\n[engine.settings]\nexec = [\"true\"]\nrestart = true\n";
    std::fs::write(manifest, text)?;
    env.omime(&["reload"])?;
    Ok(())
}

fn wait_for_saved(env: &TestEnv, pid: u32) -> anyhow::Result<()> {
    wait_until("the old engine to enter its finalizer", || {
        std::fs::read_to_string(env.dir().join("state/engine-saved"))
            .ok()?
            .lines()
            .any(|line| line == pid.to_string())
            .then_some(())
    })
}

fn wait_for_replacement(env: &TestEnv, old: u32) -> anyhow::Result<()> {
    wait_until_within(
        Duration::from_secs(6),
        "the replacement engine to register",
        || (env.engine_pid("saver")? != old && env.count_in_log("engine ready") >= 2).then_some(()),
    )
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn typing_during_settings_reload_is_delivered_before_the_engine_exits() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&["--hang"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    enable_saver_settings(&env)?;
    let old = env.engine_pid("saver").expect("the saving engine");
    let editor = env.editor(&[])?;
    env.type_on_seat("seat0", "before")?;
    editor.wait_for_text("before")?;
    env.omime(&["settings", "saver"])?;
    env.wait_for_log("restarting the engine to read its settings")?;
    wait_for_saved(&env, old)?;
    env.type_on_seat("seat0", "during")?;
    wait_until_within(
        Duration::from_millis(500),
        "direct typing while the engine saves",
        || (editor.text() == "beforeduring").then_some(()),
    )?;
    assert_eq!(
        env.engine_pid("saver"),
        Some(old),
        "the finalizer must still be running"
    );
    let engines = env.omime(&["engines"])?;
    assert!(
        engines
            .lines()
            .any(|line| line.contains("saver") && line.ends_with("stopping")),
        "{engines}"
    );
    wait_for_replacement(&env, old)?;
    env.type_on_seat("seat0", "after")?;
    editor.wait_for_text("beforeduringafter")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn settings_reload_releases_pending_fcitx_keys_and_ignores_late_commits() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&["--hang", "--hold-key"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    enable_saver_settings(&env)?;
    // Give this deliberately blocked RPC its own budget. The assertion below is
    // much earlier, so the ordinary key watchdog cannot make it pass.
    env.configure("[engine]\nkey_timeout_ms = 3000\n")?;
    let old = env.engine_pid("saver").expect("the saving engine");
    let client = env.fcitx_client(&[])?;
    client.send("focus-in")?;
    client.wait_for("the field to focus", |line| line == "ok")?;
    std::fs::write(env.dir().join("state/hold-next-key"), b"hold")?;
    let from = client.lines().len();
    client.send("key 0x61 38 0 press")?;
    wait_until("the engine to hold the key", || {
        env.dir().join("state/key-in-flight").exists().then_some(())
    })?;
    env.omime(&["settings", "saver"])?;
    env.wait_for_log("restarting the engine to read its settings")?;
    wait_until_within(
        Duration::from_millis(500),
        "the outstanding Fcitx reply",
        || {
            client.lines()[from..]
                .iter()
                .any(|line| line == "reply false")
                .then_some(())
        },
    )?;
    std::fs::write(env.dir().join("state/release-key"), b"release")?;
    env.wait_for_log("discarding a late answer")?;
    wait_for_saved(&env, old)?;
    client.send("key 0x62 56 0 press")?;
    wait_until_within(
        Duration::from_millis(500),
        "a Fcitx key during the stop",
        || {
            (client.lines()[from..]
                .iter()
                .filter(|line| line.as_str() == "reply false")
                .count()
                == 2)
                .then_some(())
        },
    )?;
    assert_eq!(
        &client.lines()[from..],
        ["reply false", "reply false"],
        "the stale handled reply must not commit or finish a key twice"
    );
    assert!(
        !env.daemon_log().contains("did not answer in time"),
        "the transition, not the watchdog, must release pending keys"
    );
    wait_for_replacement(&env, old)?;
    client.send("key 0x63 54 0 press")?;
    client.wait_for("the replacement engine's commit", |line| {
        line == "commit \"c\""
    })?;
    client.wait_for("the replacement engine's key reply", |line| {
        line == "reply true"
    })?;
    assert_eq!(
        &client.lines()[from..],
        ["reply false", "reply false", "commit \"c\"", "reply true"]
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn settings_reload_releases_queued_keys_before_a_late_reply() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&["--hang", "--hold-key"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    enable_saver_settings(&env)?;
    env.configure("[engine]\nkey_timeout_ms = 3000\n")?;
    let old = env.engine_pid("saver").expect("the saving engine");
    let editor = env.editor(&[])?;
    std::fs::write(env.dir().join("state/hold-next-key"), b"hold")?;
    env.type_on_seat("seat0", "ab")?;
    wait_until("the engine to hold the first key", || {
        env.dir().join("state/key-in-flight").exists().then_some(())
    })?;
    assert_eq!(editor.text(), "");
    env.omime(&["settings", "saver"])?;
    env.wait_for_log("restarting the engine to read its settings")?;
    wait_until_within(
        Duration::from_millis(500),
        "the outstanding and queued keys",
        || (editor.text() == "ab").then_some(()),
    )?;
    std::fs::write(env.dir().join("state/release-key"), b"release")?;
    env.wait_for_log("discarding a late answer")?;
    wait_for_saved(&env, old)?;
    wait_for_replacement(&env, old)?;
    env.type_on_seat("seat0", "c")?;
    editor.wait_for_text("abc")?;
    assert!(!env.daemon_log().contains("did not answer in time"));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn native_engine_can_finish_shutdown() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[saving_engine(&[])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("x")?;
    editor.wait_for_text("x")?;
    let pid = env.engine_pid("saver").expect("the saving engine");
    env.signal_daemon(rustix::process::Signal::TERM)?;
    assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
    wait_for_engine_exit(pid)?;
    assert_saved(&env, pid);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_native_engine_ignoring_shutdown_is_killed_after_the_grace_period() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[saving_engine(&["--hang"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let pid = env.engine_pid("saver").expect("the hanging engine");
    let start = Instant::now();
    env.signal_daemon(rustix::process::Signal::TERM)?;
    assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
    wait_for_engine_exit(pid)?;
    assert_saved(&env, pid);
    assert!(
        start.elapsed() >= Duration::from_secs(4),
        "Shutdown and SIGTERM must each get their two-second grace period"
    );
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "shutdown exceeded its bound"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn shutdown_has_one_deadline_when_hosted_startup_stalls() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[hosted_engine(&[])])?;
    let wrappers = env.dir().join("wrappers");
    std::fs::create_dir(&wrappers)?;
    let wrapper = wrappers.join("dbus-daemon");
    // Only the hosted bus uses this PATH override: the harness's private session
    // bus is already running. Stall authentication after advertising a real socket
    // so the hosted supervisor remains alive beyond the first stop deadline.
    std::fs::write(
        &wrapper,
        r#"#!/usr/bin/env python3
import os, signal, socket
runtime = os.environ['XDG_RUNTIME_DIR']
path = os.path.join(runtime, 'oh-my-ime', 'hosted', 'bus')
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(8)
print('unix:path=' + path, flush=True)
client, _ = server.accept()
open(os.path.join(runtime, 'bus-stalled'), 'w').close()
while True:
    signal.pause()
"#,
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
    let log = env.dir().join("stalled-startup-daemon.log");
    let mut paths = vec![wrappers];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let programs = program_dir();
    let wrapped = programs.join(".omimed-wrapped");
    // This fault-injection test must substitute dbus-daemon. Nix's launch wrapper
    // deliberately puts the real bus first, so exercise its exact installed
    // daemon here with the package's data directory preserved. Ordinary installed
    // tests still enter through the launch wrapper and validate that packaging.
    let mut command = if wrapped.is_file() {
        let mut command = env.command(&wrapped);
        let data = env.command("printenv").arg("XDG_DATA_DIRS").output()?;
        let data = String::from_utf8(data.stdout)?;
        let mut data_dirs: Vec<_> = std::env::split_paths(data.trim()).collect();
        data_dirs.push(programs.parent().expect("bin has a parent").join("share"));
        command.env("XDG_DATA_DIRS", std::env::join_paths(data_dirs)?);
        command
    } else {
        env.command(programs.join("omimed"))
    };
    if std::env::var_os("OMIME_E2E_INSTALL_DIR").is_some() {
        command.env_remove("LD_LIBRARY_PATH");
    }
    // This custom launch keeps the PATH override out of every other test. Reap
    // the daemon on failures too; its private bus has the normal TERM disposition.
    struct Daemon(Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut daemon = Daemon(
        command
            .args(["--engine", "hosted"])
            .env("OMIME_LOG", "debug")
            .env("PATH", std::env::join_paths(paths)?)
            .stderr(Stdio::from(std::fs::File::create(&log)?))
            .spawn()?,
    );
    wait_until("the hosted bus to stall authentication", || {
        env.dir().join("bus-stalled").exists().then_some(())
    })?;
    let start = Instant::now();
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&daemon.0),
        rustix::process::Signal::TERM,
    )?;
    let status = wait_until_within(
        Duration::from_secs(12),
        "the stopping daemon to exit",
        || daemon.0.try_wait().ok().flatten(),
    )?;
    let elapsed = start.elapsed();
    let log = std::fs::read_to_string(log)?;
    eprintln!("stalled hosted startup shutdown: {elapsed:?}; {status}\n{log}");
    assert!(status.success(), "{log}");
    assert!(
        elapsed >= Duration::from_secs(5),
        "the probe must reach the stop deadline: {elapsed:?}\n{log}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "the daemon restarted its five-second stop deadline: {elapsed:?}\n{log}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hotplug_during_shutdown_keeps_direct_typing_working() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[saving_engine(&["--hang"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("before")?;
    editor.wait_for_text("before")?;
    let grabs = env.count_in_log("keyboard grabbed");
    env.signal_daemon(rustix::process::Signal::TERM)?;
    env.wait_for_log("shutting down")?;
    let output = env
        .command("swaymsg")
        .args(["seat", "shutdown-new-seat", "fallback", "false"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "cannot add the private seat: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // Allow the new wl_seat global to reach the bridge during its shutdown grace.
    std::thread::sleep(Duration::from_millis(250));
    // Target the original seat explicitly: after hotplug, wtype may choose the
    // newly advertised seat, which intentionally has no focused editor.
    env.type_on_seat("seat0", "after")?;
    wait_until_within(
        Duration::from_secs(1),
        "direct typing during shutdown",
        || (editor.text() == "beforeafter").then_some(()),
    )?;
    assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
    assert_eq!(
        env.count_in_log("keyboard grabbed"),
        grabs,
        "shutdown recreated an input grab\n{}",
        env.daemon_log()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_hung_native_finalizer_does_not_outlive_omimed() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[saving_engine(&["--hang"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let raw_pid = env.engine_pid("saver").expect("the hanging engine");
    let pid = rustix::process::Pid::from_raw(raw_pid.try_into()?).expect("engine pid");
    // Always clean up the deliberately hung child if an assertion fails.
    struct KillOnDrop(rustix::process::Pid);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = rustix::process::kill_process(self.0, rustix::process::Signal::KILL);
        }
    }
    let _cleanup = KillOnDrop(pid);
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    wait_until("the engine to hang after its save", || {
        std::fs::read_to_string(env.dir().join("state/engine-saved"))
            .ok()?
            .lines()
            .any(|line| line == raw_pid.to_string())
            .then_some(())
    })?;
    env.signal_daemon(rustix::process::Signal::KILL)?;
    assert!(!env.wait_for_daemon_exit(Duration::from_secs(2))?.success());
    wait_until_within(
        Duration::from_secs(2),
        "the orphaned finalizer to stop",
        || {
            // After its parent dies, the test cannot reap it. A zombie is already
            // dead; its new parent is responsible for collecting the exit status.
            match std::fs::read_to_string(format!("/proc/{raw_pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(()),
                Ok(stat)
                    if stat
                        .split_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with("Z ")) =>
                {
                    Some(())
                }
                _ => None,
            }
        },
    )?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn reload_and_restart_stop_native_engines_politely() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&["--slow"])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let first = env.engine_pid("saver").expect("the saving engine");
    let manifest = env.dir().join("engines/saver/engine.toml");
    let mut text = std::fs::read_to_string(&manifest)?;
    text += "\n[engine.settings]\nexec = [\"true\"]\nrestart = true\n";
    std::fs::write(manifest, text)?;
    env.omime(&["reload"])?;
    env.omime(&["settings", "saver"])?;
    wait_until("the engine to reload its settings", || {
        env.engine_pid("saver").filter(|pid| *pid != first)
    })?;
    wait_for_engine_exit(first)?;
    assert_saved(&env, first);
    let second = env.engine_pid("saver").expect("the reloaded engine");
    let editor = env.editor(&[])?;
    for letter in ["a", "b", "c"] {
        env.type_text(letter)?;
        // Let the late answer and Reset finish before the next key, so each
        // press exercises another timeout instead of passing through in a burst.
        std::thread::sleep(Duration::from_millis(300));
    }
    editor.wait_for_text("abc")?;
    env.wait_for_log("the engine keeps timing out; restarting it")?;
    wait_for_engine_exit(second)?;
    assert_saved(&env, second);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn uninstall_stops_native_engines_politely() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&[])], Some("saver"))?;
    env.wait_for_log("engine ready")?;
    let pid = env.engine_pid("saver").expect("the saving engine");
    std::fs::remove_dir_all(env.dir().join("engines/saver"))?;
    env.omime(&["reload"])?;
    wait_for_engine_exit(pid)?;
    assert_saved(&env, pid);
    Ok(())
}

// Use a private ID so removing its manifest also uninstalls the engine when the
// programs under test come from a package that bundles Echo's own manifest.
fn socket_engine() -> TestEngine {
    TestEngine {
        id: "socket-probe",
        ..echo_engine()
    }
}

fn wait_for_ready(env: &TestEnv, id: &str) -> anyhow::Result<()> {
    wait_until("the engine to register", || {
        env.omime(&["engines"])
            .ok()?
            .lines()
            .any(|line| line.split_whitespace().any(|field| field == id) && line.ends_with("ready"))
            .then_some(())
    })
}

fn fd_count(pid: u32) -> anyhow::Result<usize> {
    Ok(std::fs::read_dir(format!("/proc/{pid}/fd"))?.count())
}

/// Include unlinked listening sockets: checking path existence alone misses the
/// descriptor leak caused by rebinding the same name on every installation.
fn engine_listener_counts(env: &TestEnv) -> anyhow::Result<[usize; 2]> {
    let inodes: std::collections::HashSet<String> =
        std::fs::read_dir(format!("/proc/{}/fd", env.daemon_pid()))?
            .filter_map(|entry| {
                let link = std::fs::read_link(entry.ok()?.path()).ok()?;
                Some(
                    link.to_str()?
                        .strip_prefix("socket:[")?
                        .strip_suffix(']')?
                        .to_owned(),
                )
            })
            .collect();
    let paths = ["engine", "wayland"].map(|name| {
        env.dir()
            .join("oh-my-ime/socket-probe")
            .join(name)
            .to_string_lossy()
            .into_owned()
    });
    let mut counts = [0, 0];
    for line in std::fs::read_to_string("/proc/net/unix")?.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() >= 8 && fields[3] == "00010000" && inodes.contains(fields[6]) {
            for (path, count) in paths.iter().zip(&mut counts) {
                if fields[7] == path {
                    *count += 1;
                }
            }
        }
    }
    Ok(counts)
}

fn idle_peer(env: &TestEnv, id: &str) -> anyhow::Result<UnixStream> {
    let socket = UnixStream::connect(env.dir().join("oh-my-ime").join(id).join("engine"))?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

fn peer_closed(mut socket: &UnixStream) -> bool {
    match socket.read(&mut [0; 1]) {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
        ),
        _ => false,
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_removal_releases_its_listening_sockets() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    let manifest = env.dir().join("engines/socket-probe/engine.toml");
    let text = std::fs::read_to_string(&manifest)?;
    let mut counts = Vec::new();
    for _ in 0..8 {
        let old = env.engine_pid("socket-probe").expect("the running engine");
        std::fs::remove_file(&manifest)?;
        env.omime(&["reload"])?;
        wait_for_engine_exit(old)?;
        // Allow the socket and child-output tasks to observe the exit.
        std::thread::sleep(Duration::from_millis(120));
        counts.push(fd_count(env.daemon_pid())?);
        std::fs::write(&manifest, &text)?;
        env.omime(&["reload"])?;
        env.omime(&["switch", "socket-probe"])?;
        wait_for_ready(&env, "socket-probe")?;
    }
    eprintln!("daemon FD counts after each uninstall: {counts:?}");
    assert!(
        counts[7] <= counts[0] + 2,
        "removed engines leaked descriptors: {counts:?}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn removed_engines_keep_no_listening_sockets() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    let manifest = env.dir().join("engines/socket-probe/engine.toml");
    let text = std::fs::read_to_string(&manifest)?;
    for cycle in 0..8 {
        assert_eq!(engine_listener_counts(&env)?, [1, 1]);
        let old = env.engine_pid("socket-probe").expect("the running engine");
        std::fs::remove_file(&manifest)?;
        env.omime(&["reload"])?;
        wait_for_engine_exit(old)?;
        wait_until_within(Duration::from_secs(1), "both listeners to close", || {
            (engine_listener_counts(&env).ok()? == [0, 0]).then_some(())
        })?;
        for name in ["engine", "wayland"] {
            assert!(!env.dir().join("oh-my-ime/socket-probe").join(name).exists());
        }
        std::fs::write(&manifest, &text)?;
        env.omime(&["reload"])?;
        env.omime(&["switch", "socket-probe"])?;
        wait_for_ready(&env, "socket-probe")?;
        eprintln!(
            "cycle {cycle}: listeners {:?}",
            engine_listener_counts(&env)?
        );
    }
    assert_eq!(engine_listener_counts(&env)?, [1, 1]);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn idle_engine_handshakes_are_bounded_or_expire() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    let editor = env.editor(&[])?;
    let before = fd_count(env.daemon_pid())?;
    let idle: Vec<_> = (0..128)
        .map(|_| idle_peer(&env, "socket-probe"))
        .collect::<Result<_, _>>()?;
    wait_until_within(
        Duration::from_secs(1),
        "excess silent peers to close",
        || (idle.iter().filter(|peer| !peer_closed(peer)).count() <= 16).then_some(()),
    )?;
    let retained = idle.iter().filter(|peer| !peer_closed(peer)).count();
    eprintln!(
        "{retained}/128 silent peers retained; daemon FDs {} -> {}",
        before,
        fd_count(env.daemon_pid())?
    );
    assert!(
        retained > 0,
        "ordinary handshakes must have time to complete"
    );
    // Socket, cancellation duplicate, and (on recent Linux) zbus's peer pidfd.
    assert!(fd_count(env.daemon_pid())? <= before + 16 * 3);
    env.type_text("during ")?;
    editor.wait_for_text("DURING ")?;
    wait_until_within(Duration::from_secs(6), "silent handshake deadlines", || {
        idle.iter().all(peer_closed).then_some(())
    })?;
    wait_until("the expired peers' descriptors to close", || {
        (fd_count(env.daemon_pid()).ok()? <= before).then_some(())
    })?;

    // A real SDK engine must also register with silent peers still connected.
    let idle: Vec<_> = (0..8)
        .map(|_| idle_peer(&env, "socket-probe"))
        .collect::<Result<_, _>>()?;
    let old = env.engine_pid("socket-probe").expect("the running engine");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(old.try_into()?).expect("pid"),
        rustix::process::Signal::KILL,
    )?;
    wait_until("a replacement process", || {
        env.engine_pid("socket-probe").filter(|pid| *pid != old)
    })?;
    wait_for_ready(&env, "socket-probe")?;
    assert!(idle.iter().any(|peer| !peer_closed(peer)));
    env.type_text("after ")?;
    editor.wait_for_text("DURING AFTER ")?;
    Ok(())
}

async fn unregistered_peer(env: &TestEnv) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::address(
        format!(
            "unix:path={}/oh-my-ime/socket-probe/engine",
            env.dir().display()
        )
        .as_str(),
    )
    .p2p()
    .build()
    .await
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn authenticated_engine_peers_are_bounded_until_registration() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut peers = Vec::new();
            for _ in 0..16 {
                peers.push(unregistered_peer(&env).await?);
            }
            let extra =
                tokio::time::timeout(Duration::from_secs(1), unregistered_peer(&env)).await?;
            assert!(
                extra.is_err(),
                "authentication alone must not release the quota"
            );
            tokio::time::timeout(Duration::from_secs(6), async {
                for peer in &peers {
                    peer.closed().await;
                }
            })
            .await?;
            // Permits become available again after those connections expire.
            let fresh =
                tokio::time::timeout(Duration::from_secs(1), unregistered_peer(&env)).await??;
            fresh.close().await?;
            anyhow::Ok(())
        })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn removing_engine_closes_pending_peers_before_readding() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let peer = unregistered_peer(&env).await?;
            let idle = idle_peer(&env, "socket-probe")?;
            let manifest = env.dir().join("engines/socket-probe/engine.toml");
            let text = std::fs::read_to_string(&manifest)?;
            std::fs::remove_file(&manifest)?;
            env.omime(&["reload"])?;
            tokio::time::timeout(Duration::from_secs(1), peer.closed()).await?;
            wait_until_within(
                Duration::from_secs(1),
                "the silent peer to close on removal",
                || peer_closed(&idle).then_some(()),
            )?;
            std::fs::write(&manifest, text)?;
            env.omime(&["reload"])?;
            env.omime(&["switch", "socket-probe"])?;
            wait_for_ready(&env, "socket-probe")?;
            let host = zbus::Proxy::new(
                &peer,
                "io.github.ign1x.OhMyIme",
                "/io/github/ign1x/OhMyIme",
                "io.github.ign1x.OhMyIme.EngineHost1",
            )
            .await?;
            let result: zbus::Result<std::collections::HashMap<String, zbus::OwnedVariant>> = host
                .call(
                    "Register",
                    &(
                        "socket-probe",
                        1u16,
                        0u16,
                        std::collections::HashMap::<String, zbus::OwnedVariant>::new(),
                    ),
                )
                .await;
            assert!(
                result.is_err(),
                "a removed peer registered with the new installation"
            );
            anyhow::Ok(())
        })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn readded_engine_ignores_the_removed_process_exit() -> anyhow::Result<()> {
    let env = TestEnv::start(&[saving_engine(&["--hang"])], Some("saver"))?;
    wait_for_ready(&env, "saver")?;
    let old = env.engine_pid("saver").expect("the running engine");
    let manifest = env.dir().join("engines/saver/engine.toml");
    let text = std::fs::read_to_string(&manifest)?;
    std::fs::remove_file(&manifest)?;
    env.omime(&["reload"])?;
    wait_for_saved(&env, old)?;
    std::fs::write(&manifest, text)?;
    env.omime(&["reload"])?;
    env.omime(&["switch", "saver"])?;
    wait_for_ready(&env, "saver")?;
    let replacement = env.engine_pid("saver").expect("the replacement");
    assert_ne!(old, replacement);
    wait_for_engine_exit(old)?;
    std::thread::sleep(Duration::from_millis(150));
    wait_for_ready(&env, "saver")?;
    assert_eq!(env.engine_pid("saver"), Some(replacement));
    let editor = env.editor(&[])?;
    env.type_text("alive")?;
    editor.wait_for_text("alive")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_accept_errors_back_off_and_recover() -> anyhow::Result<()> {
    let env = TestEnv::start(&[socket_engine()], Some("socket-probe"))?;
    wait_for_ready(&env, "socket-probe")?;
    let pid = rustix::process::Pid::from_raw(env.daemon_pid().try_into()?).expect("pid");
    let limits_text = std::fs::read_to_string(format!("/proc/{}/limits", env.daemon_pid()))?;
    let hard = limits_text
        .lines()
        .find(|line| line.starts_with("Max open files"))
        .and_then(|line| line.split_whitespace().nth(4))
        .expect("the hard descriptor limit")
        .parse()?;
    let limits = rustix::process::prlimit(
        Some(pid),
        rustix::process::Resource::Nofile,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(hard),
        },
    )?;
    let idle = idle_peer(&env, "socket-probe")?;
    let ui = UnixStream::connect(env.dir().join("oh-my-ime/socket-probe/wayland"))?;
    env.wait_for_log("Too many open files")?;
    std::thread::sleep(Duration::from_millis(1300));
    // Restore before using the CLI or performing assertions, including on base.
    rustix::process::prlimit(Some(pid), rustix::process::Resource::Nofile, limits)?;
    let warnings = env
        .daemon_log()
        .lines()
        .filter(|line| line.contains("accepting an") || line.contains("accepting a UI"))
        .count();
    eprintln!("accept warnings in 1.3 s of descriptor exhaustion: {warnings}");
    assert!(
        warnings <= 2,
        "accept failures produced {warnings} warnings"
    );
    drop((idle, ui));
    env.omime(&["status"])?;
    let old = env.engine_pid("socket-probe").expect("the running engine");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(old.try_into()?).expect("pid"),
        rustix::process::Signal::KILL,
    )?;
    wait_until("the engine to restart after descriptor exhaustion", || {
        env.engine_pid("socket-probe").filter(|pid| *pid != old)
    })?;
    wait_for_ready(&env, "socket-probe")?;
    Ok(())
}

fn wait_for_count(env: &TestEnv, needle: &str, count: usize) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while env.count_in_log(needle) < count {
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} × {needle:?} in the omimed log"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_slow_engine_cannot_hold_up_typing() -> anyhow::Result<()> {
    // The engine answers every key after 450 ms; omimed waits 200 ms.
    let env = TestEnv::start(&[dummy(&["--slow-ms", "450"])], Some("dummy"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("abc")?;
    editor.wait_for_text("abc")?;
    env.wait_for_log("did not answer in time")?;
    // Late answers are discarded, so nothing is typed twice.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(editor.text(), "abc");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_slow_activation_keeps_the_first_keys_in_the_engine() -> anyhow::Result<()> {
    // Loading a schema for a new field can exceed the ordinary 200 ms key budget.
    let env = TestEnv::start(&[dummy(&["--slow-activate-ms", "450"])], Some("dummy"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.wait_for_log("activate frontend=")?;
    env.type_text("abc")?;
    editor.wait_for_text("abc")?;
    // Each press and release must have reached the engine, in place of bypassing
    // it while Activate is still loading the schema.
    wait_for_count(&env, "processed key", 6);
    assert_eq!(env.count_in_log("did not answer in time"), 0);
    assert_eq!(env.count_in_log("engine launched engine=\"dummy\""), 1);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_stuck_activation_returns_keys_and_restarts_its_engine() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy(&["--slow-activate-ms", "10000"]), echo_engine()],
        Some("dummy"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.wait_for_log("activate frontend=")?;
    env.type_text("abc")?;
    editor.wait_for_text("abc")?;
    env.wait_for_log("the engine did not recover after a timeout; restarting it")?;
    env.omime(&["switch", "echo"])?;
    env.type_text("de ")?;
    editor.wait_for_text("abcDE ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn invalid_viewport_only_disconnects_its_engine() -> anyhow::Result<()> {
    let marker = std::env::temp_dir().join(format!(
        "omime-e2e-misbehaved-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let marker_arg = marker.display().to_string();
    // The bridge rejects the invalid viewport before it reaches the compositor.
    // The restarted engine behaves; the daemon keeps its original connection.
    let env = TestEnv::start(
        &[dummy(&[
            "--misbehave",
            "viewport",
            "--misbehave-once",
            &marker_arg,
        ])],
        Some("dummy"),
    )?;
    env.wait_for_log("invalid viewport destination")?;
    wait_for_count(&env, "engine ready", 2);

    let editor = env.editor(&[])?;
    env.type_text("still typing")?;
    editor.wait_for_text("still typing")?;
    assert_eq!(env.count_in_log("lost the compositor connection"), 0);
    let _ = std::fs::remove_file(marker);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn an_upstream_protocol_fault_stops_its_engine_before_the_daemon() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy(&["--misbehave", "subsurface"]), echo_engine()],
        Some("dummy"),
    )?;
    // wl-proxy may observe HUP before the compositor's error message. In that
    // case the process crash budget must still stop the engine before the global
    // reconnect budget can end the daemon.
    wait_until_within(
        std::time::Duration::from_secs(45),
        "the faulty engine to stop",
        || {
            let log = env.daemon_log();
            (log.contains("quarantining an engine")
                || log.contains("engine keeps crashing; giving up"))
            .then_some(())
        },
    )?;
    wait_for_count(&env, "connected to the compositor", 2);
    let editor = env.editor(&[])?;
    env.type_text("plain")?;
    editor.wait_for_text("plain")?;
    env.omime(&["switch", "echo"])?;
    env.type_text("works ")?;
    editor.wait_for_text("plainWORKS ")?;
    assert!(env.count_in_log("engine launched engine=\"dummy\"") <= 6);
    assert!(
        !env.daemon_log()
            .contains("the connection keeps failing; giving up")
    );
    Ok(())
}
