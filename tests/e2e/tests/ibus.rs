//! End-to-end tests with a real IBus, hosted by omimed as an engine through
//! `omime-ibus`.
//!
//! They use the `ibus-daemon` on `$PATH` (the dev shells have IBus with libpinyin), and
//! pass without testing anything when IBus is not installed. See `typing.rs` for how to
//! run them.

use omime_e2e::harness::{TestEngine, TestEnv, program_dir, wait_until};

/// IBus as a hosted engine, if it is installed.
fn ibus() -> Option<TestEngine> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("ibus-daemon"))
        .find(|path| path.is_file())?;
    Some(TestEngine {
        id: "ibus",
        program: program_dir().join("omime-ibus"),
        args: Vec::new(),
        hosted: true,
    })
}

/// Chooses `engines` in IBus, as `ibus-setup` does: `preload-engines` in the user's dconf
/// database, which the test environment has its own of.
fn choose_engines(env: &TestEnv, engines: &[&str]) -> anyhow::Result<()> {
    let mut table = gvdb::write::HashTableBuilder::new();
    let engines: Vec<String> = engines.iter().map(|e| (*e).to_owned()).collect();
    table.insert("/desktop/ibus/general/preload-engines", engines)?;
    let data = gvdb::write::FileWriter::new().write_to_vec_with_table(table)?;
    let dir = env.dir().join("config/dconf");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("user"), data)?;
    Ok(())
}

/// Switches to the hosted IBus and waits until it serves the seat.
fn switch_to_ibus(env: &TestEnv) -> anyhow::Result<()> {
    env.omime(&["switch", "ibus"])?;
    env.wait_for_log("hosted input method ready")?;
    Ok(())
}

/// The engine the hosted IBus uses, as `ibus engine` reports it. The tool finds the
/// hosted daemon like any other: omime-ibus writes the address file of the session's
/// display.
fn current_engine(env: &TestEnv) -> Option<String> {
    let output = env.command("ibus").arg("engine").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Picks libpinyin for the focused field, as a user would with `ibus engine`. The
/// command cannot report success: after switching it runs `setxkbmap`, which fails
/// without an X display. The engine the daemon reports is the truth.
fn select_libpinyin(env: &TestEnv) -> anyhow::Result<()> {
    env.command("ibus").args(["engine", "libpinyin"]).output()?;
    wait_until("libpinyin to be the engine", || {
        (current_engine(env).as_deref() == Some("libpinyin")).then_some(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_wayland_applications() -> anyhow::Result<()> {
    let Some(engine) = ibus() else {
        eprintln!("IBus is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    switch_to_ibus(&env)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    select_libpinyin(&env)?;
    // Keys go in with real key codes: wtype uploads a keymap of its own for every
    // string, whose made-up key codes a hosted IBus decodes with a stale keymap. Space
    // picks the first candidate; libpinyin keeps the space.
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    // The candidate window of IBus's panel was shown as a popup.
    env.wait_for_log("hosted input method popup created")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_xim_applications() -> anyhow::Result<()> {
    let Some(engine) = ibus() else {
        eprintln!("IBus is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start_with_x11(&[engine], None)?;
    switch_to_ibus(&env)?;
    let client = env.xim_client(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    select_libpinyin(&env)?;
    env.type_on_seat("seat0", "nihao ")?;
    client.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    client.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_shipped_manifest_installs_ibus() -> anyhow::Result<()> {
    if ibus().is_none() {
        eprintln!("IBus is not installed; nothing to test");
        return Ok(());
    }
    let env = TestEnv::start(&[], None)?;
    // The manifest users install, as the repository ships it: it runs the omime-ibus
    // on $PATH.
    let source = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/engines/ibus");
    let output = env.omime(&["install", source])?;
    assert!(output.contains("Installed ibus"), "{output}");
    switch_to_ibus(&env)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // Without an engine picked, IBus passes keys on.
    env.type_on_seat("seat0", "ok")?;
    editor.wait_for_text("ok")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_shows_ibus_engines() -> anyhow::Result<()> {
    let Some(engine) = ibus() else {
        eprintln!("IBus is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    let tray = env.tray_host()?;
    switch_to_ibus(&env)?;
    let _editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    select_libpinyin(&env)?;
    // IBus's panel shows the engine's icon in its item; omimed's item shows it too.
    tray.wait_for_latest("libpinyin's icon", "icon ", |i| i.contains("pinyin"))?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn ibus_engines_are_chosen_directly() -> anyhow::Result<()> {
    let Some(engine) = ibus() else {
        eprintln!("IBus is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    choose_engines(&env, &["xkb:us::eng", "libpinyin"])?;
    switch_to_ibus(&env)?;
    // The engines chosen in IBus are what users choose from, keyboard layouts are not.
    let list = wait_until("libpinyin in omime list", || {
        let list = env.omime(&["list"]).ok()?;
        list.contains("ibus/libpinyin").then_some(list)
    })?;
    assert!(!list.contains("xkb:"), "{list}");
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // Choosing libpinyin through omimed selects it in IBus.
    env.omime(&["switch", "ibus/libpinyin"])?;
    env.wait_for_status_starting("ibus/libpinyin")?;
    wait_until("IBus to use libpinyin", || {
        (current_engine(&env).as_deref() == Some("libpinyin")).then_some(())
    })?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    Ok(())
}
