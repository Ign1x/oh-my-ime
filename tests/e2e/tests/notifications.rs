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
    assert_eq!(shown[1].1, shown[0].0, "in place of the first: {shown:?}");
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
