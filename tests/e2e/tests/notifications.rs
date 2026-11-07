//! End-to-end tests of the desktop notifications omimed shows about trouble, served by
//! `omime-notifications` as a desktop's notification daemon would. See `typing.rs` for
//! how to run them.

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use omime_e2e::harness::{TestEngine, TestEnv, echo_engine, target_dir, wait_until};
use rustix::process::Signal;

/// The notifications shown so far: (id, the id they replace, "summary|body").
fn shown(lines: &[String]) -> Vec<(u32, u32, String)> {
    lines
        .iter()
        .filter_map(|line| {
            let mut parts = line.strip_prefix("notify ")?.splitn(3, ' ');
            let id = parts.next()?.parse().ok()?;
            let replaces = parts.next()?.parse().ok()?;
            Some((id, replaces, parts.next()?.to_owned()))
        })
        .collect()
}

/// Whether `line` says `english`, or `chinese` under a Chinese locale.
fn says(line: &str, english: &str, chinese: &str) -> bool {
    line.contains(english) || line.contains(chinese)
}

/// Writes config.toml without reloading.
fn write_config(env: &TestEnv, text: &str) -> anyhow::Result<()> {
    std::fs::write(env.dir().join("config/oh-my-ime/config.toml"), text)?;
    Ok(())
}

/// Reloads with SIGHUP, which nobody answers, and waits until omimed did.
fn reload(env: &TestEnv, times: usize) -> anyhow::Result<()> {
    env.signal_daemon(Signal::HUP)?;
    wait_until("the reload", || {
        (env.count_in_log("reloading on SIGHUP") >= times).then_some(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_mistake_in_config_toml_is_told_once_until_it_is_fixed() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let server = env.notifications()?;
    write_config(&env, "[tray]\nenabled = \"yes\"\n")?;
    reload(&env, 1)?;
    let told = server.wait_for("the notification", |l| l.starts_with("notify "))?;
    assert!(told.contains("config.toml"), "{told}");
    assert!(
        says(&told, "keeps the configuration", "继续使用之前的配置"),
        "{told}"
    );
    // Told once: the same mistake again is not news.
    reload(&env, 2)?;
    // Fixed, the notification goes; broken again, it is news again.
    write_config(&env, "[panel]\nenabled = false\n")?;
    reload(&env, 3)?;
    server.wait_for("the notification to go", |l| l.starts_with("close "))?;
    write_config(&env, "[apps]\nremember = 1\n")?;
    reload(&env, 4)?;
    wait_until("the second notification", || {
        (shown(&server.lines()).len() >= 2).then_some(())
    })?;
    let shown = shown(&server.lines());
    assert_eq!(shown.len(), 2, "{:?}", server.lines());
    assert_eq!(
        shown[1].1, 0,
        "a new notification after resolution: {shown:?}"
    );
    assert_ne!(shown[1].0, shown[0].0, "the old ID stays closed: {shown:?}");
    server.send(&format!("click {}", shown[1].0))?;
    env.wait_for_log("opened the settings")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn closed_notification_actions_are_ignored() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    let server = env.notifications()?;
    write_config(&env, "[apps]\nremember = 'bad'\n")?;
    let (status, _) = env.omime_status(&["reload"])?;
    assert!(!status.success());
    let told = server.wait_for("the configuration notification", |l| {
        l.starts_with("notify ")
    })?;
    let (id, _, _) = shown(&[told]).pop().context("a notification")?;
    env.configure("")?;
    server.wait_for("the resolved notification to close", |l| {
        l == format!("close {id}")
    })?;
    server.send(&format!("click {id}"))?;
    server.wait_for("the stale action to be sent", |l| {
        l == format!("clicked {id}")
    })?;
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !env.daemon_log().contains("opened the settings"),
        "a stale ActionInvoked for a closed notification launched settings\n{}",
        env.daemon_log()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn dismissed_and_expired_notification_actions_are_ignored() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    let server = env.notifications()?;
    for reason in [1, 2] {
        write_config(&env, "[apps]\nremember = 'bad'\n")?;
        let (status, _) = env.omime_status(&["reload"])?;
        assert!(!status.success());
        let id = wait_until("a new configuration notification", || {
            let notifications = shown(&server.lines());
            (notifications.len() == reason as usize).then(|| notifications.last().unwrap().0)
        })?;
        // No delay between the close and the stale click: their bus order matters.
        server.send(&format!("closed {id} {reason}\nclick {id}"))?;
        server.wait_for("the stale action to be sent", |l| {
            l == format!("clicked {id}")
        })?;
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !env.daemon_log().contains("opened the settings"),
            "ActionInvoked after NotificationClosed({id}, {reason}) launched settings\n{}",
            env.daemon_log()
        );
        // Closing a notification does not resolve its trouble or reset its rate limit.
        let (status, _) = env.omime_status(&["reload"])?;
        assert!(!status.success());
        env.configure("")?;
    }
    assert_eq!(shown(&server.lines()).len(), 2, "{:?}", server.lines());
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_live_click_before_notification_closed_opens_the_settings_once() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    let server = env.notifications()?;
    write_config(&env, "[apps]\nremember = 'bad'\n")?;
    let (status, _) = env.omime_status(&["reload"])?;
    assert!(!status.success());
    let told = server.wait_for("the configuration notification", |l| {
        l.starts_with("notify ")
    })?;
    let (id, _, _) = shown(&[told]).pop().context("a notification")?;
    // Desktops normally emit the action before dismissing a notification. An
    // unknown ID and a later stale action must not open additional windows.
    server.send(&format!(
        "click 0\nclick {}\nclick {id}\nclosed {id} 2\nclick {id}",
        id + 1
    ))?;
    env.wait_for_log("opened the settings")?;
    wait_until("both actions for the notification to be sent", || {
        (server
            .lines()
            .iter()
            .filter(|l| **l == format!("clicked {id}"))
            .count()
            == 2)
            .then_some(())
    })?;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        env.count_in_log("opened the settings"),
        1,
        "{}",
        env.daemon_log()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_closure_burst_before_notify_reply_does_not_stall_later_notifications() -> anyhow::Result<()> {
    closure_burst_recovery("notify")
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_closure_burst_before_close_reply_does_not_stall_later_notifications() -> anyhow::Result<()> {
    closure_burst_recovery("close")
}

fn closure_burst_recovery(method: &str) -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[])?;
    let missing = env.dir().join("nonexistent-engine");
    assert!(!missing.exists());
    let engine_dir = env.dir().join("engines/missing");
    std::fs::create_dir_all(&engine_dir)?;
    // A target/debug program gets an `env LD_LIBRARY_PATH=...` launcher during
    // installed tests. Use an absent private path so spawn itself must fail.
    std::fs::write(
        engine_dir.join("engine.toml"),
        format!(
            "[engine]\nid = 'missing'\nname = 'missing'\napi = '1.0'\nexec = [{:?}]\n",
            missing.display().to_string()
        ),
    )?;
    env.launch_daemon()?;
    let server = env.notifications()?;
    // Keep an unrelated notification live. Its later click proves the ordered
    // stream has drained the burst before another notification is requested.
    let (status, _) = env.omime_status(&["switch", "missing"])?;
    assert!(!status.success());
    let told = server.wait_for("the engine notification", |l| l.starts_with("notify "))?;
    let (live, _, _) = shown(&[told]).pop().context("the engine notification")?;
    env.wait_for_log("told the user")?;

    let break_config = || -> anyhow::Result<()> {
        write_config(&env, "[apps]\nremember = 'bad'\n")?;
        let (status, _) = env.omime_status(&["reload"])?;
        assert!(!status.success());
        Ok(())
    };
    if method == "close" {
        break_config()?;
        wait_until("both notifications to arrive", || {
            (env.count_in_log("told the user") == 2).then_some(())
        })?;
    }
    // zbus's signal queue holds 64 messages. All 65 closures precede the method
    // reply on the same connection, so waiting without a timeout cannot finish.
    server.send(&format!("burst {method} 65"))?;
    server.wait_for("the closure burst to be armed", |l| {
        l == format!("burst {method} 65 armed")
    })?;
    if method == "notify" {
        break_config()?;
    } else {
        env.configure("")?;
    }
    server.wait_for("the closure burst before the reply", |l| {
        l == format!("burst {method} 65 sent")
    })?;
    assert!(env.omime_status(&["status"])?.0.success());
    server.send(&format!("click {live}"))?;
    env.wait_for_log("opened the settings")
        .with_context(|| format!("notifier stalled after closure burst before {method} reply"))?;

    // A timed-out Notify may already be displayed with an unknown ID. Resolve
    // that trouble and require a fresh notification whose live action works.
    env.configure("")?;
    break_config()?;
    let notifications = wait_until("a fresh notification after the closure burst", || {
        let notifications = shown(&server.lines());
        (notifications.len() == 3).then_some(notifications)
    })?;
    let (fresh, replaces, _) = &notifications[2];
    assert_eq!(*replaces, 0, "{notifications:?}");
    assert_ne!(*fresh, notifications[1].0, "{notifications:?}");
    server.send(&format!("click {fresh}"))?;
    wait_until("the fresh notification's live action", || {
        (env.count_in_log("opened the settings") == 2).then_some(())
    })?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn an_engine_that_does_not_start_is_told_and_the_click_opens_the_settings() -> anyhow::Result<()> {
    // It never registers.
    let sleepy = TestEngine {
        id: "sleepy",
        program: PathBuf::from("sleep"),
        args: vec!["60".to_owned()],
        env: Vec::new(),
        hosted: false,
    };
    let env = TestEnv::start(&[sleepy, echo_engine()], None)?;
    let server = env.notifications()?;
    env.omime(&["switch", "sleepy"])?;
    let told = server.wait_for("the notification", |l| l.starts_with("notify "))?;
    assert!(
        says(
            &told,
            "sleepy stopped|It did not start in time.",
            "sleepy已停止|它没能及时启动。"
        ),
        "{told}"
    );
    let (id, _, _) = shown(&[told]).pop().context("a notification")?;
    server.send(&format!("click {id}"))?;
    env.wait_for_log("opened the settings")?;
    wait_until("the settings window", || {
        env.has_window("io.github.ign1x.OhMyIme.Settings")
            .ok()?
            .then_some(())
    })?;
    Ok(())
}

/// A process that is killed when dropped.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn another_input_method_on_the_seat_is_told_until_omimed_has_the_seat() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    let server = env.notifications()?;
    // An input method of its own on the compositor, as a Fcitx 5 started by the
    // desktop would be.
    let log = "other-im.log";
    let mut other = Process(
        env.command(target_dir().join("omime-im-client"))
            .args(["--log", log])
            .stdout(Stdio::null())
            .spawn()
            .context("cannot start omime-im-client")?,
    );
    wait_until("the other input method", || {
        std::fs::read_to_string(env.dir().join(log))
            .ok()?
            .lines()
            .any(|l| l == "ready")
            .then_some(())
    })?;
    env.launch_daemon()?;
    let told = server.wait_for("the notification", |l| l.starts_with("notify "))?;
    assert!(
        says(
            &told,
            "Another input method is running",
            "另一个输入法正在运行"
        ),
        "{told}"
    );
    assert!(says(&told, "seat seat0", "座位 seat0"), "{told}");

    // Once it is gone, omimed takes the seat and the notification goes.
    let _ = other.0.kill();
    let _ = other.0.wait();
    let editor = env.editor(&[])?;
    wait_until_within_retries(&env)?;
    server.wait_for("the notification to go", |l| l.starts_with("close "))?;
    env.type_text("ok")?;
    editor.wait_for_text("ok")?;
    assert_eq!(shown(&server.lines()).len(), 1, "{:?}", server.lines());
    Ok(())
}

/// Waits until omimed asked for the seat again and grabbed its keyboard, which takes a
/// retry or two.
fn wait_until_within_retries(env: &TestEnv) -> anyhow::Result<()> {
    omime_e2e::harness::wait_until_within(Duration::from_secs(20), "omimed on the seat", || {
        env.daemon_log().contains("keyboard grabbed").then_some(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn notifications_can_be_turned_off() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let server = env.notifications()?;
    env.configure("[notifications]\nenabled = false\n")?;
    // Broken, the file still has notifications off: omimed keeps what it had.
    write_config(&env, "[notifications]\nenabled = \"no\"\n")?;
    reload(&env, 1)?;
    env.wait_for_log("keeping the configuration in effect")?;
    std::thread::sleep(Duration::from_millis(500));
    assert!(shown(&server.lines()).is_empty(), "{:?}", server.lines());
    Ok(())
}
