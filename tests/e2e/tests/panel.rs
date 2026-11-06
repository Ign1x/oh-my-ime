//! End-to-end tests of the switcher, `omime-panel`, which omimed starts with
//! `[panel] enabled`. See `typing.rs` for how to run them.

use omime_e2e::harness::{TestEnv, echo_engine, wait_until, wait_until_within};
use std::time::Duration;

/// Just inside the top edge of the switcher with two choices, at the bottom centre of
/// the 1280 × 720 output: 120 pixels above the bottom, 108 high.
const INSIDE: (u32, u32) = (640, 720 - 120 - 108 + 4);

fn start(config: &str) -> anyhow::Result<TestEnv> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.configure(&format!("{config}[panel]\nenabled = true\n"))?;
    env.wait_for_log("omime_panel: waiting for switches")?;
    Ok(env)
}

/// Lines of the switcher's log about switches, shown or not.
fn switches_seen(env: &TestEnv) -> usize {
    env.count_in_log("showing the switcher")
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_switcher_shows_after_hotkeys_and_commands() -> anyhow::Result<()> {
    let env = start("")?;
    let _editor = env.editor(&[])?;
    env.wait_for_log("keyboard grabbed")?;
    let (x, y) = INSIDE;
    let before = env.pixel(x, y)?;

    env.key(&["ctrl"], "space")?;
    env.wait_for_log("showing the switcher input_method=\"echo\" reason=\"hotkey\"")?;
    wait_until("the switcher", || {
        (env.pixel(x, y).ok()? != before).then_some(())
    })?;
    wait_until_within(Duration::from_secs(5), "the switcher to go", || {
        (env.pixel(x, y).ok()? == before).then_some(())
    })?;

    env.omime(&["toggle"])?;
    env.wait_for_log("reason=\"command\"")?;
    wait_until("the switcher again", || {
        (env.pixel(x, y).ok()? != before).then_some(())
    })?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn applications_get_their_input_method_back_without_the_switcher() -> anyhow::Result<()> {
    let env = start("[apps]\nremember = true\n")?;
    let _a = env.editor(&["--app-id", "editor-a"])?;
    env.wait_for_log("focused application changed app=\"editor-a\"")?;
    env.omime(&["switch", "echo"])?;
    let _b = env.editor(&["--app-id", "editor-b"])?;
    env.wait_for_log("focused application changed app=\"editor-b\"")?;
    env.omime(&["switch", "direct"])?;
    wait_until("both switches", || (switches_seen(&env) == 2).then_some(()))?;

    // editor-a gets echo back, quietly: omimed does not even announce it.
    env.focus("editor-a")?;
    wait_until("echo for editor-a", || {
        env.omime(&["status"])
            .ok()?
            .starts_with("echo")
            .then_some(())
    })?;
    env.omime(&["switch", "echo"])?;
    let again = "showing the switcher input_method=\"echo\" reason=\"command\"";
    wait_until("the switch", || {
        (env.count_in_log(again) == 2).then_some(())
    })?;
    // Signals come in order: one for the focus change would have come before.
    assert_eq!(switches_seen(&env), 3, "{}", env.daemon_log());
    Ok(())
}

struct ChooserProcess {
    child: std::process::Child,
    log: std::path::PathBuf,
}
impl Drop for ChooserProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl ChooserProcess {
    fn start(env: &TestEnv, tag: &str) -> anyhow::Result<Self> {
        let log = env.dir().join(format!("chooser-{tag}.log"));
        let child = env
            .command(omime_e2e::harness::program_dir().join("omime"))
            .arg("choose")
            .env("OMIME_LOG", "debug")
            .stderr(std::fs::File::create(&log)?)
            .spawn()?;
        let mut this = Self { child, log };
        wait_until("chooser keyboard focus", || {
            if this.child.try_wait().ok()?.is_some() {
                return None;
            }
            std::fs::read_to_string(&this.log)
                .ok()?
                .contains("overlay keyboard focused")
                .then_some(())
        })
        .map_err(|e| {
            anyhow::anyhow!(
                "{e:#}\n{}",
                std::fs::read_to_string(&this.log).unwrap_or_default()
            )
        })?;
        Ok(this)
    }
    fn finished(&mut self) -> anyhow::Result<()> {
        let status = wait_until("chooser to exit", || self.child.try_wait().ok().flatten())?;
        anyhow::ensure!(
            status.success(),
            "chooser exited {status}: {}",
            std::fs::read_to_string(&self.log)?
        );
        Ok(())
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn choosing_with_keyboard_remembers_the_original_app_and_escape_cancels() -> anyhow::Result<()> {
    // Explicit selection also works when the passive switch notification is disabled.
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.configure("[apps]\nremember = true\n")?;
    let editor = env.editor(&["--app-id", "chooser-editor"])?;
    let mut chooser = ChooserProcess::start(&env, "keyboard")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        env.screenshot(&env.dir().join("chooser.png"))?;
    }
    env.key(&[], "Down")?;
    assert_eq!(
        env.omime(&["status"])?.trim(),
        "direct input",
        "browsing must not switch"
    );
    env.key(&[], "Return")?;
    chooser.finished()?;
    env.wait_for_status_starting("echo")?;
    let apps = env.omime(&["apps"])?;
    assert!(
        apps.lines()
            .any(|l| l.starts_with("chooser-editor") && l.ends_with("echo")),
        "{apps}"
    );
    env.type_text("ab ")?;
    editor.wait_for_text("AB ")?;
    let mut chooser = ChooserProcess::start(&env, "cancel")?;
    env.key(&[], "Home")?;
    env.key(&[], "Escape")?;
    chooser.finished()?;
    env.wait_for_status_starting("echo")?;
    env.type_text("cd ")?;
    editor.wait_for_text("AB CD ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn choosing_with_pointer_and_reopening_does_not_start_two_choosers() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[])?;
    let mut chooser = ChooserProcess::start(&env, "pointer")?;
    let status = env
        .command(omime_e2e::harness::program_dir().join("omime"))
        .arg("choose")
        .status()?;
    assert!(status.success());
    // Two rows: 196px high, centered. Direct input is the first row.
    env.click(640, 328)?;
    chooser.finished()?;
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    env.type_text("plain")?;
    editor.wait_for_text("plain")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn chooser_closes_if_the_daemon_goes_away() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let _editor = env.editor(&[])?;
    let mut chooser = ChooserProcess::start(&env, "daemon")?;
    env.signal_daemon(rustix::process::Signal::TERM)?;
    chooser.finished()?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_chooser_handles_large_lists_and_live_removal() -> anyhow::Result<()> {
    let engine = omime_e2e::harness::dummy_engine(&[
        "--input-methods",
        "one,two,three,four,five,six,seven,eight,nine,ten",
    ]);
    let env = TestEnv::start(&[engine], Some("dummy"))?;
    let _editor = env.editor(&[])?;
    env.wait_for_status_starting("dummy/one")?;
    let mut chooser = ChooserProcess::start(&env, "large")?;
    env.key(&[], "End")?;
    // Highlighting the last item still leaves the current method untouched.
    env.wait_for_status_starting("dummy/one")?;
    env.key(&["shift"], "Tab")?;
    env.key(&[], "Return")?;
    chooser.finished()?;
    env.wait_for_status_starting("dummy/nine")?;

    let mut chooser = ChooserProcess::start(&env, "scroll")?;
    env.command(omime_e2e::harness::target_dir().join("omime-pointer"))
        .args(["HEADLESS-1", "640", "290", "scroll-down"])
        .status()?;
    env.key(&[], "Return")?;
    chooser.finished()?;
    env.wait_for_status_starting("dummy/ten")?;

    let mut chooser = ChooserProcess::start(&env, "removal")?;
    env.omime(&["disable", "dummy"])?;
    wait_until("only direct input in the chooser", || {
        std::fs::read_to_string(&chooser.log)
            .ok()?
            .contains("chooser catalog updated count=1")
            .then_some(())
    })?;
    env.key(&[], "Home")?;
    env.key(&[], "Return")?;
    chooser.finished()?;
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    Ok(())
}
