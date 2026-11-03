//! End-to-end test for `examples/python-gtk4`, a Python + GTK 4 engine.
//!
//! Run it like the other end-to-end tests:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e --test python -- --ignored
//! ```
//!
//! The example needs PyGObject and GTK 4; inside another environment the test passes
//! without testing anything.

use std::path::PathBuf;
use std::process::Command;

use anyhow::Context as _;
use omime_e2e::harness::{TestEngine, TestEnv};

/// The green the example paints its window with.
const GREEN: u32 = 0x2e7d32;

/// The example's script. Integration tests run in the `tests/e2e` package directory.
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/python-gtk4/shout.py")
        .canonicalize()
        .expect("examples/python-gtk4/shout.py is missing")
}

/// Whether this environment can run the example: Python with PyGObject and GTK 4.
fn gtk4_is_available() -> bool {
    let check = Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '4.0'); from gi.repository import Gtk",
        ])
        .output();
    match check {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "python3 cannot import GTK 4, skipping: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            false
        }
        Err(error) => {
            eprintln!("cannot run python3, skipping: {error}");
            false
        }
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_python_gtk4_engine_composes_shows_a_window_and_commits() -> anyhow::Result<()> {
    if !gtk4_is_available() {
        return Ok(());
    }

    // omimed would set GDK_BACKEND itself; GSK_RENDERER=cairo is needed because the
    // test's headless sway renders with pixman and offers no GPU buffers.
    let engine = TestEngine {
        id: "shout",
        program: PathBuf::from("python3"),
        args: vec![script().display().to_string()],
        env: vec![
            ("GSK_RENDERER".into(), "cairo".into()),
            ("GDK_BACKEND".into(), "wayland".into()),
            // The test session has no accessibility bus, and GTK would only warn.
            ("GTK_A11Y".into(), "none".into()),
        ],
        hosted: false,
    };

    let env = TestEnv::start(&[engine], None)?;
    env.omime(&["switch", "shout"])?;
    env.wait_for_log("engine ready")?;
    env.wait_for_log("engine window created")?;
    let editor = env.editor(&[])?;

    env.type_text("hello")?;
    editor
        .wait_for("the preedit `hello`", |l| {
            l.starts_with("preedit \"hello\"")
        })
        .with_context(|| format!("the editor said {:?}", editor.lines()))?;

    // The popup opens below the text cursor: x = 12 + 8 × column, y = 20, height 18 in
    // the test editor. The window is 240 × 48, so this point is inside it whether the
    // cursor rectangle includes the preedit (column 5) or not (column 0), and away
    // from the centred text.
    env.wait_for_pixel(57, 43, GREEN)?;

    // Space commits in UPPERCASE and still reaches the application.
    env.key(&[], "space")?;
    editor.wait_for_text("HELLO ")?;

    // A click on the window commits too. The window follows the text cursor, now at
    // column 11 while "world" is being composed; this point is inside it either way.
    env.type_text("world")?;
    editor.wait_for("the preedit `world`", |l| {
        l.starts_with("preedit \"world\"")
    })?;
    env.wait_for_pixel(110, 43, GREEN)?;
    env.click(110, 43)?;
    editor.wait_for_text("HELLO WORLD")?;
    Ok(())
}
