//! End-to-end tests with a real Fcitx5, hosted by omimed as an engine.
//!
//! They use the `fcitx5` on `$PATH` with a configuration of their own (the test
//! environment has its own `XDG_CONFIG_HOME`), and pass without testing anything when
//! Fcitx5 is not installed. See `typing.rs` for how to run them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use omime_e2e::harness::{TIMEOUT, TestEngine, TestEnv};

/// Fcitx5 as a hosted engine, if it is installed.
fn fcitx5() -> Option<TestEngine> {
    let program = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("fcitx5"))
        .find(|path| path.is_file())?;
    Some(TestEngine {
        id: "fcitx5",
        program,
        args: Vec::new(),
        hosted: true,
    })
}

/// A profile with the US keyboard only, so that typing gives what is typed.
const PROFILE: &str = "\
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=keyboard-us

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[GroupOrder]
0=Default
";

/// Gives Fcitx5 [`PROFILE`], in the environment's configuration directory.
fn write_profile(env: &TestEnv) -> anyhow::Result<()> {
    let config: PathBuf = env.dir().join("config/fcitx5");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("profile"), PROFILE)?;
    Ok(())
}

/// A profile with Pinyin, active from the start (omimed takes Ctrl+Space, Fcitx5's
/// trigger key).
const PINYIN_PROFILE: &str = "\
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=pinyin

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=pinyin
Layout=

[GroupOrder]
0=Default
";

/// Gives Fcitx5 [`PINYIN_PROFILE`] and switches to it.
fn start_pinyin(env: &TestEnv) -> anyhow::Result<()> {
    let config: PathBuf = env.dir().join("config/fcitx5");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("profile"), PINYIN_PROFILE)?;
    std::fs::write(config.join("config"), "[Behavior]\nActiveByDefault=True\n")?;
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_wayland_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    start_pinyin(&env)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // Space picks the first candidate.
    env.type_text("nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn no_key_stays_held_after_shortcuts() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    start_pinyin(&env)?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // Shortcuts applications use, and keys Fcitx5 takes for itself: Ctrl+Shift and
    // Ctrl+; are Fcitx5 hotkeys, a lone Shift switches its mode.
    for (modifiers, key) in [
        (&["ctrl"][..], "a"),
        (&["ctrl"][..], "c"),
        (&["ctrl", "shift"][..], "Shift_L"),
        (&["ctrl"][..], "semicolon"),
        (&[][..], "Shift_L"),
        (&[][..], "Shift_L"),
        (&["ctrl"][..], "space"),
        (&["ctrl"][..], "space"),
    ] {
        env.key(modifiers, key)?;
    }
    std::thread::sleep(Duration::from_millis(500));
    let lines = editor.lines();
    let held = lines.iter().rev().find(|l| l.starts_with("held"));
    let modifiers = lines.iter().rev().find(|l| l.starts_with("modifiers "));
    assert!(
        held.is_none_or(|l| l == "held "),
        "keys stayed held: {held:?}\n{lines:?}"
    );
    assert!(
        modifiers.is_none_or(|l| l.starts_with("modifiers 0 0 ")),
        "modifiers stayed held: {modifiers:?}\n{lines:?}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_shows_fcitx5s_input_method() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    let tray = env.tray_host()?;
    start_pinyin(&env)?;
    let _editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // omimed's item names the input method in use inside Fcitx 5, and shows the icon
    // of Fcitx 5's own item, from Fcitx 5's icons.
    let tooltip = tray.wait_for_latest("Pinyin in the tooltip", "tooltip ", |t| {
        (t.starts_with("Pinyin") || t.starts_with("拼音")) && t.ends_with("(fcitx5)")
    })?;
    // The icon comes from Fcitx 5's item, which may report a moment later.
    tray.wait_for_latest(&format!("Fcitx 5's icon for {tooltip:?}"), "icon ", |i| {
        i.contains("pinyin")
    })?;
    tray.wait_for_latest("Fcitx 5's icons", "theme ", |t| t.ends_with("share/icons"))?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_xim_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start_with_x11(&[engine], None)?;
    start_pinyin(&env)?;
    let client = env.xim_client(&[])?;
    env.type_on_seat("seat0", "nihao ")?;
    client.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    client.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_fcitx_module_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    start_pinyin(&env)?;
    let client = env.fcitx_client(&[])?;
    client.send("focus-in")?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // n i h a o space, by keyval and X11 key code.
    for (keyval, keycode) in [
        (0x6e, 57),
        (0x69, 31),
        (0x68, 43),
        (0x61, 38),
        (0x6f, 32),
        (0x20, 65),
    ] {
        client.send(&format!("key {keyval:#x} {keycode} 0 press"))?;
        client.send(&format!("key {keyval:#x} {keycode} 0 release"))?;
    }
    client.wait_for("the commit", |l| l == "commit \"你好\"")?;
    // Fcitx5 does not consume releases and sends them back; no press may reach the
    // application, which would type the letter.
    let lines = client.lines();
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("forward ") && l.ends_with(" false")),
        "keys leaked to the application: {lines:?}"
    );
    Ok(())
}

/// Starts the environment and Fcitx5 with [`PROFILE`].
fn start(engine: TestEngine) -> anyhow::Result<TestEnv> {
    let env = TestEnv::start(&[engine], None)?;
    write_profile(&env)?;
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    Ok(env)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_shipped_manifest_installs_fcitx5() -> anyhow::Result<()> {
    if fcitx5().is_none() {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    }
    let env = TestEnv::start(&[], None)?;
    write_profile(&env)?;
    // The manifest users install, as the repository ships it.
    let source = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/engines/fcitx5");
    let output = env.omime(&["install", source])?;
    assert!(output.contains("Installed fcitx5"), "{output}");
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    env.type_text("ok")?;
    editor.wait_for_text("ok")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn fcitx5_types_through_omimed() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = start(engine)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    env.type_text("hello")?;
    editor.wait_for_text("hello")?;

    // Ctrl+Shift+U types a character by its code point: Fcitx5 composes it in the
    // preedit, shows its window and commits the character.
    env.key(&["ctrl", "shift"], "u")?;
    env.type_text("4e2d")?;
    editor.wait_for("the code point in the preedit", |l| {
        l.starts_with("preedit") && l.contains("4e2d")
    })?;
    env.key(&[], "space")?;
    editor.wait_for_text("hello中")?;

    // Ctrl+Alt+Shift+U searches characters: the candidates appear in Fcitx5's window,
    // and Return picks the first (for "4e00", the Kangxi radical ⼀ comes before 一).
    env.key(&["ctrl", "alt", "shift"], "u")?;
    env.wait_for_log("hosted input method popup created")?;
    env.type_text("4e00")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        std::thread::sleep(Duration::from_millis(300));
        env.screenshot(&env.dir().join("fcitx5.png"))?;
    }
    env.key(&[], "Return")?;
    let deadline = Instant::now() + TIMEOUT;
    while editor.text().chars().count() < "hello中".chars().count() + 1 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "nothing was picked: the editor has {:?}",
            editor.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = editor.text();
    assert!(
        text.starts_with("hello中") && !text.contains("4e00"),
        "{text:?}"
    );
    Ok(())
}

/// Xiaohe shuangpin and pinyin, with Xiaohe's keys.
const SHUANGPIN_PROFILE: &str = "\
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=pinyin

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=pinyin
Layout=

[Groups/0/Items/2]
Name=shuangpin
Layout=

[GroupOrder]
0=Default
";

/// Waits until `omime list` shows `id`, and returns the list.
fn wait_for_method(env: &TestEnv, id: &str) -> anyhow::Result<String> {
    omime_e2e::harness::wait_until(&format!("{id} in omime list"), || {
        let list = env.omime(&["list"]).ok()?;
        list.lines()
            .any(|l| l.split_whitespace().nth(l.starts_with('*') as usize) == Some(id))
            .then_some(list)
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_input_methods_inside_fcitx5_are_chosen_directly() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let mut env = TestEnv::start(&[engine], None)?;
    let config: PathBuf = env.dir().join("config/fcitx5");
    std::fs::create_dir_all(config.join("conf"))?;
    std::fs::write(config.join("profile"), SHUANGPIN_PROFILE)?;
    std::fs::write(config.join("config"), "[Behavior]\nActiveByDefault=True\n")?;
    std::fs::write(config.join("conf/pinyin.conf"), "ShuangpinProfile=Xiaohe\n")?;
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;

    // Fcitx 5's input methods are what users choose from; its keyboard layout is not
    // one, direct input is.
    let list = wait_for_method(&env, "fcitx5/shuangpin")?;
    assert!(list.contains("fcitx5/pinyin"), "{list}");
    assert!(!list.contains("keyboard-us"), "{list}");

    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    env.omime(&["switch", "fcitx5/shuangpin"])?;
    env.wait_for_status_starting("fcitx5/shuangpin")?;
    // Xiaohe: ni hao is n i h c.
    env.type_on_seat("seat0", "nihc ")?;
    editor.wait_for_text("你好")?;

    // The next input method is pinyin, inside the same Fcitx 5.
    env.omime(&["next"])?;
    env.wait_for_status_starting("fcitx5/pinyin")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    editor.wait_for_text("你好中国")?;

    // After a restart omimed lists them before Fcitx 5 is up, and gives Fcitx 5 back
    // the input method that was in use.
    env.omime(&["switch", "fcitx5/shuangpin"])?;
    env.wait_for_status_starting("fcitx5/shuangpin")?;
    env.restart_daemon()?;
    wait_for_method(&env, "fcitx5/pinyin")?;
    env.wait_for_status_starting("fcitx5/shuangpin")?;
    Ok(())
}
