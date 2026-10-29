//! End-to-end tests with two seats, each with its own keyboard, focus and input method
//! session.
//!
//! See `typing.rs` for how to run them.

use std::time::{Duration, Instant};

use omime_e2e::harness::{TIMEOUT, TestEnv, echo_engine};

/// Waits up to `timeout` until omimed's log contains `needle` `count` times.
fn log_reaches(env: &TestEnv, needle: &str, count: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while env.count_in_log(needle) < count {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn each_seat_types_into_its_own_field() -> anyhow::Result<()> {
    const SEAT0: &str = "keyboard grabbed seat=\"seat0\"";
    const SEAT1: &str = "keyboard grabbed seat=\"seat1\"";
    let env = TestEnv::start_with_seats(&[echo_engine()], Some("echo"), &["seat1"])?;

    // Two windows side by side on the 1280×720 output: `a` on the left, `b` on the
    // right, each drawing 320×120 pixels in its top left corner. `a` uses seat0, `b`
    // seat1.
    let a = env.editor(&["--app-id", "a"])?;
    let b = env.spawn_editor(&["--app-id", "b", "--seat", "seat1"])?;
    // New windows get the focus of every seat, so `b` has seat1's focus once it is up.
    b.wait_for("b to get seat1's focus", |l| l == "ready")?;
    let grabs = env.count_in_log(SEAT0);
    assert!(log_reaches(&env, SEAT1, 1, TIMEOUT));
    // seat0 clicks `a` to take it back. Sway arranges the windows asynchronously and an
    // early click misses, so it clicks until seat0 holds `a`. (`b` is not clicked: a
    // click on the window a seat already focuses makes sway drop its keyboard focus.)
    for attempt in 1.. {
        env.click_on_seat("seat0", 100, 50)?;
        if log_reaches(&env, SEAT0, grabs + 1, Duration::from_secs(1)) {
            break;
        }
        assert!(attempt < 5, "seat0 never took `a`");
    }

    // wtype picks a seat of its own, so both seats type with omime-keyboard.
    env.type_on_seat("seat0", "ab ")?;
    a.wait_for_text("AB ")?;
    env.type_on_seat("seat1", "cd ")?;
    b.wait_for_text("CD ")?;

    // Each field only got its own seat's text.
    env.type_on_seat("seat1", "ef ")?;
    b.wait_for_text("CD EF ")?;
    assert_eq!(a.text(), "AB ");
    Ok(())
}
