//! End-to-end tests of the settings, `omime-settings`. See `typing.rs` for how to run
//! them.

use std::path::PathBuf;
use std::process::{Child, Stdio};

use anyhow::Context as _;
use omime_e2e::harness::{TestEnv, echo_engine, program_dir, sparkle_engine, wait_until};

/// The settings' application id, which compositors see.
const APP_ID: &str = "io.github.ign1x.OhMyIme.Settings";

/// "Settings…" in the tray menu, in English or Chinese after the locale.
const TRAY_ENTRY: [&str; 2] = ["Settings…", "设置…"];

/// The settings window. Closed when dropped.
struct Settings {
    child: Child,
    log: PathBuf,
}

impl Settings {
    /// Opens the settings with `program` (`omime settings`, or `omime-settings PAGE`).
    fn open(env: &TestEnv, program: &str, args: &[&str]) -> anyhow::Result<Self> {
        let log = env.dir().join(format!("{program}.log"));
        let child = env
            .command(program_dir().join(program))
            .args(args)
            .env("OMIME_LOG", "info")
            .env("SLINT_BACKEND", "winit-software")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log)?)
            .spawn()
            .with_context(|| format!("cannot run {program}"))?;
        let settings = Self { child, log };
        wait_until("the settings window", || {
            env.has_window(APP_ID).ok()?.then_some(())
        })?;
        Ok(settings)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Waits until the settings logged `needle` `count` times.
    fn wait_for_log(&self, needle: &str, count: usize) -> anyhow::Result<()> {
        wait_until(&format!("{needle:?} in the settings' log"), || {
            (self.log().matches(needle).count() >= count).then_some(())
        })
        .with_context(|| self.log())
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_settings_show_omimed_and_follow_it_as_it_stops_and_starts() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine()], None)?;
    let settings = Settings::open(&env, "omime", &["settings"])?;
    settings.wait_for_log("read the settings input_methods=", 1)?;

    env.restart_daemon()?;
    settings.wait_for_log("omimed is not running", 1)?;
    settings.wait_for_log("read the settings input_methods=", 2)?;
    assert!(env.has_window(APP_ID)?, "the window stays open");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_add_dialog_enables_offered_demo_engines() -> anyhow::Result<()> {
    for id in ["echo", "sparkle"] {
        let env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
        let list = env.omime(&["list"])?;
        assert!(!list.contains(id), "{list}");
        let settings = Settings::open(&env, "omime", &["settings"])?;
        settings.wait_for_log("read the settings input_methods=", 1)?;
        // With no enabled methods, the first button opens the add dialog. Its
        // search field takes focus; the matching entry's Add button follows it.
        env.key(&[], "Tab")?;
        env.key(&[], "space")?;
        env.type_text(id)?;
        env.key(&[], "Tab")?;
        env.key(&[], "space")?;
        wait_until("the demo to be enabled in settings", || {
            let config =
                std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml")).ok()?;
            config
                .contains(&format!("engines = [\"{id}\"]"))
                .then_some(())
        })
        .with_context(|| settings.log())?;
        let list = env.omime(&["list"])?;
        assert!(list.contains(id), "{list}");
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn options_change_in_the_window() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    assert!(env.omime(&["config"])?.contains("tray.enabled = true"));
    let settings = Settings::open(&env, "omime-settings", &["general"])?;
    settings.wait_for_log("read the settings", 1)?;
    // The keyboard reaches the first switch of the page, the tray's, and flips it.
    env.key(&[], "Tab")?;
    env.key(&[], "space")?;
    wait_until("the tray to be off", || {
        let config = env.omime(&["config"]).ok()?;
        config.contains("tray.enabled = false").then_some(())
    })
    .with_context(|| settings.log())?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_menu_opens_the_settings() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let tray = env.tray_host()?;
    let menu = tray.wait_for_latest("the settings in the menu", "menu ", |m| {
        TRAY_ENTRY
            .iter()
            .any(|entry| m.ends_with(&format!("|-|{entry}")))
    })?;
    let entry = TRAY_ENTRY
        .iter()
        .find(|entry| menu.ends_with(*entry))
        .context("no entry")?;
    tray.send(&format!("click {entry}"))?;
    env.wait_for_log("opened the settings")?;
    wait_until("the settings window", || {
        env.has_window(APP_ID).ok()?.then_some(())
    })?;
    Ok(())
}
