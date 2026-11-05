//! Check locale selection in fresh CLI processes, without changing the test runner's
//! environment or needing an installed system locale.

use std::process::{Command, Output};

fn omime(locale: &[(&str, &str)], args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omime"))
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env_remove("LANGUAGE")
        .env("LANG", "en_US.UTF-8")
        .envs(locale.iter().copied())
        .args(args)
        .output()
        .expect("run omime")
}

#[test]
fn help_follows_message_locale_and_language_preferences() {
    for locale in [
        vec![("LC_ALL", "zh_CN.UTF-8")],
        vec![("LC_ALL", ""), ("LC_MESSAGES", "zh-Hans")],
        vec![("LANGUAGE", "fr:zh_CN:en")],
    ] {
        let output = omime(&locale, &["help"]);
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.starts_with("用法：omime <命令>"), "{help}");
        assert!(help.contains("doctor [--app PID]"));
        assert!(help.contains("setup --undo [--dry-run]"));
        assert!(!help.contains("Usage:"));
    }
    for locale in [
        vec![("LC_ALL", "C"), ("LANGUAGE", "zh_CN")],
        vec![("LC_ALL", "POSIX"), ("LANG", "zh_CN.UTF-8")],
        vec![("LANGUAGE", "en:zh_CN")],
        vec![("LC_ALL", "de_DE.UTF-8")],
    ] {
        let output = omime(&locale, &["help"]);
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .starts_with("Usage: omime")
        );
    }
}

#[test]
fn chinese_argument_errors_keep_exit_codes_and_user_values() {
    let locale = [("LC_MESSAGES", "zh_CN.UTF-8")];
    let output = omime(&locale, &["doctor", "--app", "0"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--app 需要一个正整数进程 ID")
    );
    let output = omime(&locale, &["不存在{arg0}"]);
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("未知命令 `不存在{arg0}`"), "{error}");
    assert!(error.contains("用法：omime"));
}
