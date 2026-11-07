//! End-to-end tests for failure handling: engines that are slow or break the shared
//! Wayland connection must never stop the user from typing.
//!
//! See `typing.rs` for how to run them.

use std::os::unix::fs::PermissionsExt as _;
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
        env.command(program_dir().join("omimed"))
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
