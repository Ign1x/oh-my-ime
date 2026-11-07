//! End-to-end tests for `omime doctor`.
//!
//! See `typing.rs` for how to run them.

use std::fs;
use std::process::Child;

use anyhow::Context as _;
use omime_e2e::harness::{TestEnv, echo_engine, program_dir, sparkle_engine};

struct App(Child);
impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_checks_the_application_environment_without_exposing_other_variables() -> anyhow::Result<()>
{
    let env = TestEnv::start(&[echo_engine()], None)?;
    let app = App(env
        .command("sleep")
        .arg("60")
        .env("QT_IM_MODULE", "wayland")
        .env("XMODIFIERS", "@im=missing")
        .env("PRIVATE_TOKEN", "never-include-this")
        .spawn()?);
    let output = env
        .command(program_dir().join("omime"))
        .env("QT_IM_MODULE", "fcitx")
        .env("XMODIFIERS", "@im=fcitx")
        .args(["doctor", "--app", &app.0.id().to_string()])
        .output()?;
    let text = String::from_utf8(output.stdout)?;
    assert!(output.status.success(), "{text}");
    assert!(text.contains("QT_IM_MODULE=\"wayland\""), "{text}");
    assert!(text.contains("XMODIFIERS=\"@im=missing\""), "{text}");
    assert!(
        !text.contains("PRIVATE_TOKEN") && !text.contains("never-include-this"),
        "{text}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_explains_how_to_enable_the_offered_demos() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    for (locale, warning) in [
        (
            "C.UTF-8",
            "no engine is enabled; enable one with `omime enable <id>` or in the settings app; available engines: ",
        ),
        (
            "zh_CN.UTF-8",
            "尚未启用任何引擎；请运行 `omime enable <id>` 或在设置应用中启用；可用引擎：",
        ),
    ] {
        let output = env
            .command(program_dir().join("omime"))
            .env("LC_ALL", locale)
            .arg("doctor")
            .output()?;
        let text = String::from_utf8(output.stdout)?;
        assert!(output.status.success(), "{text}");
        let prefix = format!("! {warning}");
        let available = text
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .with_context(|| format!("no enablement guidance in:\n{text}"))?;
        let available: Vec<_> = available.split(',').map(str::trim).collect();
        for id in ["echo", "sparkle"] {
            assert!(available.contains(&id), "{id} missing from:\n{text}");
        }
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_uses_the_running_daemons_engine_list() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    env.configure("engines = [\"echo\"]\n")?;
    let empty = env.dir().join("empty");
    fs::create_dir(&empty)?;
    let output = env
        .command(program_dir().join("omime"))
        .env("OMIME_ENGINE_PATH", &empty)
        .env("XDG_DATA_DIRS", &empty)
        .args(["doctor"])
        .output()?;
    let text = String::from_utf8(output.stdout)?;
    assert!(output.status.success(), "{text}");
    assert!(
        !text.contains("engine `echo` is in `engines` but not installed"),
        "{text}"
    );
    Ok(())
}

/// A healthy private session: omimed runs, owns seat0 and has the echo engine installed.
#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn doctor_is_happy_with_a_working_setup() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.omime(&["enable", "echo"])?;
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
        stdout
            .lines()
            .any(|line| line.starts_with("✓ engines: echo")),
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
