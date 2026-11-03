//! End-to-end tests of engines' floating panels: windows of an engine on the desktop,
//! rather than at the text cursor, here Sparkle's recent-emoji panel. See `typing.rs`
//! for how to run them.

use omime_e2e::harness::{TestEnv, sparkle_engine, wait_until};

/// The panel on the 1280 × 720 output: eight tiles of 44 pixels and a padding of 8 make
/// it 368 × 60, 24 pixels from the bottom and right edges.
const PANEL: (u32, u32) = (1280 - 24 - 368, 720 - 24 - 60);

/// The centre of the panel's tile `i`.
fn tile(i: u32) -> (u32, u32) {
    (PANEL.0 + 8 + 44 * i + 22, PANEL.1 + 8 + 22)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sparkles_panel_inserts_recent_emoji_with_a_click() -> anyhow::Result<()> {
    let env = TestEnv::start(&[sparkle_engine()], Some("sparkle"))?;
    env.wait_for_log("engine window created")?;
    // Inside the panel, in its padding.
    let inside = (PANEL.0 + 4, PANEL.1 + 30);
    let empty = env.pixel(inside.0, inside.1)?;

    // Sparkle's menu turns the panel on; it shows while Sparkle serves a field.
    let menu = env.omime(&["properties"])?;
    assert!(menu.contains("recent-panel"), "{menu}");
    env.omime(&["activate", "recent-panel"])?;
    let editor = env.editor(&[])?;
    wait_until("the panel", || {
        (env.pixel(inside.0, inside.1).ok()? != empty).then_some(())
    })?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        env.screenshot(&env.dir().join("panel.png"))?;
    }

    // A click inserts an emoji, and the keyboard stays in the field.
    let (x, y) = tile(6);
    wait_until("a click on the panel", || {
        env.click(x, y).ok()?;
        editor.text().contains('🚀').then_some(())
    })?;
    // It comes first now: the first tile inserts it again.
    let (x, y) = tile(0);
    env.click(x, y)?;
    editor.wait_for_text("🚀🚀")?;
    env.type_text("ok")?;
    editor.wait_for_text("🚀🚀ok")?;
    let saved = std::fs::read_to_string(env.dir().join("state/oh-my-ime/sparkle"))?;
    assert!(saved.contains("panel = true"), "{saved}");
    assert!(saved.contains("recent = 🚀 😀"), "{saved}");

    // Without Sparkle in the field, the panel goes.
    env.omime(&["switch", "direct"])?;
    wait_until("the panel to go", || {
        (env.pixel(inside.0, inside.1).ok()? == empty).then_some(())
    })?;
    Ok(())
}
