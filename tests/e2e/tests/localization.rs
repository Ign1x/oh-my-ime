//! Localized presentation must preserve control IDs and configuration contents.

use std::fs;

use omime_e2e::harness::{TestEnv, echo_engine, program_dir};

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn chinese_session_has_translated_tray_diagnostics_and_stable_control_ids() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    let manifest = env.dir().join("engines/echo/engine.toml");
    let original = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{original}\n[engine.i18n.zh-CN]\nname = \"回声\"\n"),
    )?;
    env.set_locale("zh_CN.UTF-8");
    env.launch_daemon()?;
    let tray = env.tray_host()?;
    tray.wait_for_latest("Chinese tray menu", "menu ", |menu| {
        menu.contains("回声") && menu.contains("直接输入") && menu.contains("设置…")
    })?;
    assert!(env.omime(&["engines"])?.contains("回声"));
    let doctor = env.omime(&["doctor"])?;
    assert!(doctor.contains("omimed 正在运行"), "{doctor}");
    assert!(
        doctor.contains("座位 seat0：输入法由 oh-my-ime 提供"),
        "{doctor}"
    );
    env.omime(&["switch", "direct"])?;
    assert_eq!(env.omime(&["status"])?.trim(), "直接输入");
    let status = env.omime(&["status", "--json"])?;
    assert!(status.contains("\"engine\":null"), "{status}");
    assert!(status.contains("\"class\":\"direct\""), "{status}");
    env.omime(&["switch", "echo"])?;
    let status = env.omime(&["status", "--json"])?;
    assert!(status.contains("\"engine\":\"echo\""), "{status}");
    let result = env
        .command(program_dir().join("omime"))
        .args(["config", "engine.key_timeout_ms", "0"])
        .output()?;
    assert!(!result.status.success());
    let error = String::from_utf8(result.stderr)?;
    assert!(
        error.contains("`engine.key_timeout_ms` 必须至少为 1"),
        "{error}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn chinese_setup_and_english_undo_preserve_managed_markers() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    env.set_locale("zh_CN.UTF-8");
    let path = env.dir().join("config/hypr/hyprland.conf");
    fs::create_dir_all(path.parent().unwrap())?;
    let original = "# 用户配置\nexec-once = fcitx5 -d\n";
    fs::write(&path, original)?;
    let preview = env.omime(&["setup", "--take-over", "--dry-run"])?;
    assert!(preview.contains("将创建"), "{preview}");
    assert_eq!(fs::read_to_string(&path)?, original);
    let output = env.omime(&["setup", "--take-over"])?;
    assert!(output.contains("Hyprland 配置已就绪"), "{output}");
    let configured = fs::read_to_string(&path)?;
    assert!(configured.contains("# omime setup begin"));
    assert!(configured.contains("# omime setup disabled startup"));
    env.set_locale("C.UTF-8");
    env.omime(&["setup", "--undo"])?;
    assert_eq!(fs::read_to_string(&path)?, original);
    Ok(())
}
