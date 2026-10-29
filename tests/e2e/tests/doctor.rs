//! End-to-end tests for `omime doctor`.
//!
//! See `typing.rs` for how to run them.

use std::fs;

use omime_e2e::harness::{TestEnv, echo_engine};

/// A healthy private session: omimed runs, owns seat0 and has the echo engine installed.
#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_is_happy_with_a_working_setup() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let _editor = env.editor(&[])?;

    let (status, stdout) = env.omime_status(&["doctor"])?;
    assert_eq!(status.code(), Some(0), "doctor:\n{stdout}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with('✓') && line.contains("seat0")),
        "no seat line in:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line.contains("echo")),
        "no engine line in:\n{stdout}"
    );
    Ok(())
}

/// A broken manifest in the engine directory is a problem, so `doctor` fails.
#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_reports_a_broken_engine() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let broken = env.dir().join("engines").join("broken");
    fs::create_dir_all(&broken)?;
    // No `name`, so the manifest does not parse.
    fs::write(broken.join("engine.toml"), "[engine]\nid = \"broken\"\n")?;

    let (status, stdout) = env.omime_status(&["doctor"])?;
    assert_eq!(status.code(), Some(1), "doctor:\n{stdout}");
    let path = broken.join("engine.toml").display().to_string();
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with('✗') && line.contains(&path)),
        "no line about {path} in:\n{stdout}"
    );
    Ok(())
}
