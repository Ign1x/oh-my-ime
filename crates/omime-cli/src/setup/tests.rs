use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::sync::atomic::{AtomicU32, Ordering};

struct Fixture {
    root: PathBuf,
    locations: Locations,
    daemon: PathBuf,
    engines: Vec<manifest::InstalledEngine>,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "omime-setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let daemon = bin.join("omimed");
        fs::write(&daemon, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&daemon, fs::Permissions::from_mode(0o700)).unwrap();
        let config_dir = root.join("config with spaces/oh-my-ime");
        let state_dir = root.join("state/oh-my-ime");
        let locations = Locations {
            journal: state_dir.join("setup.toml"),
            config_dir,
            data_dir: root.join("data"),
            state_dir,
        };
        let engines = [
            include_str!("../../../../engines/echo/engine.toml"),
            include_str!("../../../../engines/sparkle/engine.toml"),
        ]
        .into_iter()
        .map(|text| {
            let (mut manifest, unknown_keys) = manifest::Manifest::parse(text).unwrap();
            manifest.engine.exec = vec![daemon.to_str().unwrap().to_owned()];
            manifest::InstalledEngine {
                manifest,
                dir: root.clone(),
                unknown_keys,
            }
        })
        .collect();
        Self {
            root,
            locations,
            daemon,
            engines,
        }
    }

    fn main(&self, ext: &str) -> PathBuf {
        self.locations
            .config_dir
            .parent()
            .unwrap()
            .join(format!("hypr/hyprland.{ext}"))
    }
    fn config(&self) -> PathBuf {
        self.locations.config_dir.join(config::CONFIG_FILE)
    }
    fn plan(&self, args: &[&str]) -> anyhow::Result<Plan> {
        prepare_with(
            &self.locations,
            &Options::parse(args)?,
            self.daemon.clone(),
            &self.engines,
        )
    }
    fn apply(&self, plan: &Plan) {
        files::apply(
            &self.locations.journal,
            plan.previous.as_ref(),
            plan.record.clone(),
            &plan.changes,
        )
        .unwrap();
    }
    fn write(&self, path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn unsupported_compositors_refuse_before_creating_any_files() {
    for args in [
        vec![],
        vec!["--start"],
        vec!["--start", "--take-over"],
        vec!["--take-over"],
    ] {
        let f = Fixture::new();
        let options = Options::parse(&args).unwrap();
        let verdict =
            crate::probe::Verdict::Unsupported(omime_proto::desktop::Capabilities::default());
        let error = run_with(&options, &f.locations, Ok(verdict))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("zwp_input_method_manager_v2"));
        assert!(!f.locations.config_dir.parent().unwrap().exists());
        assert!(!f.locations.state_dir.parent().unwrap().exists());
    }
}

#[tokio::test]
async fn a_failed_probe_refuses_before_creating_any_files() {
    let f = Fixture::new();
    let error = run_with(
        &Options::parse(&["--start", "--take-over"]).unwrap(),
        &f.locations,
        Err(anyhow::anyhow!("probe failed")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "probe failed");
    assert!(!f.locations.config_dir.parent().unwrap().exists());
    assert!(!f.locations.state_dir.parent().unwrap().exists());
}

#[test]
fn preview_is_read_only_and_enables_the_selected_engine_without_hiding_defaults() {
    let mut f = Fixture::new();
    let mut default = f.engines[0].clone();
    default.manifest.engine.id = "default".into();
    default.manifest.engine.enabled_by_default = true;
    f.engines.push(default);
    let p = f.plan(&["--engine", "echo", "--dry-run"]).unwrap();
    assert_eq!(p.changes.len(), 5);
    assert!(!f.locations.config_dir.exists());
    assert!(!f.locations.state_dir.exists());
    let text = &p
        .record
        .files
        .iter()
        .find(|file| file.path == f.config())
        .unwrap()
        .installed;
    let parsed = Config::parse(text).unwrap().0;
    assert_eq!(parsed.initial_input_method.as_deref(), Some("echo"));
    assert_eq!(parsed.engines, Some(vec!["default".into(), "echo".into()]));
}

#[test]
fn setup_explicitly_enables_only_the_chosen_demo() {
    for id in ["echo", "sparkle"] {
        for config in ["", "engines = []\n"] {
            let f = Fixture::new();
            f.write(&f.config(), config);
            f.apply(&f.plan(&["--engine", id]).unwrap());
            let parsed = Config::parse(&fs::read_to_string(f.config()).unwrap())
                .unwrap()
                .0;
            assert_eq!(parsed.initial_input_method.as_deref(), Some(id));
            assert_eq!(parsed.engines, Some(vec![id.to_owned()]));
        }
    }
}

#[test]
fn setup_updates_application_launch_environments_and_undo_keeps_other_edits() {
    let f = Fixture::new();
    let root = f.locations.config_dir.parent().unwrap();
    let uwsm = root.join("uwsm/env-hyprland");
    let systemd = root.join("environment.d/90-oh-my-ime.conf");
    let original = "# existing session\nexport QT_IM_MODULE=wayland\n";
    f.write(&uwsm, original);
    let plan = f.plan(&["--engine", "echo"]).unwrap();
    f.apply(&plan);
    let text = fs::read_to_string(&uwsm).unwrap();
    assert!(text.starts_with(original));
    assert!(text.contains("export QT_IM_MODULE='fcitx'"));
    assert!(text.contains("export QT_IM_MODULES='wayland;fcitx'"));
    assert!(
        fs::read_to_string(&systemd)
            .unwrap()
            .contains("XMODIFIERS=@im=fcitx")
    );
    f.apply(&f.plan(&[]).unwrap());
    assert_eq!(fs::read_to_string(&uwsm).unwrap(), text);
    f.write(&uwsm, &(text + "export ANOTHER_SETTING=yes\n"));
    undo(&f.locations, false).unwrap();
    assert_eq!(
        fs::read_to_string(uwsm).unwrap(),
        format!("{original}export ANOTHER_SETTING=yes\n")
    );
    assert!(!systemd.exists());
}

#[test]
fn setup_will_not_replace_an_existing_launch_environment_file() {
    let f = Fixture::new();
    let systemd = f
        .locations
        .config_dir
        .parent()
        .unwrap()
        .join("environment.d/90-oh-my-ime.conf");
    f.write(&systemd, "QT_IM_MODULE=ibus\n");
    assert!(f.plan(&[]).is_err());
    assert_eq!(fs::read_to_string(&systemd).unwrap(), "QT_IM_MODULE=ibus\n");
    assert!(!f.main("lua").exists());
    assert!(!f.locations.journal.exists());
}

#[test]
fn applying_twice_and_undoing_restores_an_empty_setup() {
    let f = Fixture::new();
    let first = f.plan(&["--engine", "echo"]).unwrap();
    f.apply(&first);
    assert!(
        Record::load(&f.locations.journal)
            .unwrap()
            .unwrap()
            .complete
    );
    assert_eq!(
        fs::read_to_string(f.main("lua"))
            .unwrap()
            .matches("omime setup begin")
            .count(),
        1
    );
    assert!(f.plan(&[]).unwrap().changes.is_empty());
    undo(&f.locations, false).unwrap();
    for file in &first.record.files {
        assert!(!file.path.exists(), "{}", file.path.display());
    }
    assert!(!f.locations.journal.exists());
    undo(&f.locations, false).unwrap();
}

#[test]
fn an_invalid_existing_config_prevents_setup_without_an_engine_option() {
    let f = Fixture::new();
    f.write(&f.config(), "engines = 42\n");
    assert!(f.plan(&[]).is_err());
    assert!(!f.main("lua").exists());
    assert!(!f.locations.journal.exists());
}

#[test]
fn takeover_is_remembered_even_when_no_file_needs_an_edit() {
    let f = Fixture::new();
    f.apply(&f.plan(&["--engine", "echo"]).unwrap());
    let takeover = f.plan(&["--take-over"]).unwrap();
    assert!(takeover.changes.is_empty());
    assert_ne!(takeover.previous.as_ref(), Some(&takeover.record));
    f.apply(&takeover);
    assert!(f.plan(&[]).unwrap().record.take_over);
    undo(&f.locations, false).unwrap();
    assert!(!f.config().exists());
}

#[test]
fn takeover_restores_existing_startups_and_file_contents_exactly() {
    for (ext, main) in [
        (
            "conf",
            "# my desktop\r\nexec-once = fcitx5 -d\r\nmonitor = , preferred, auto, 1",
        ),
        (
            "lua",
            "hl.on(\"hyprland.start\", function()\n    hl.exec_cmd(\"ibus-daemon -drx\")\nend)\n-- my desktop",
        ),
    ] {
        let f = Fixture::new();
        let path = f.main(ext);
        let original = "# my shortcuts\n[hotkeys]\ntoggle = \"Super+space\"\n";
        f.write(&path, main);
        f.write(&f.config(), original);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(f.plan(&["--engine", "echo"]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), main);
        assert!(!f.locations.journal.exists());
        let p = f.plan(&["--engine", "echo", "--take-over"]).unwrap();
        f.apply(&p);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("disabled startup 0")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        undo(&f.locations, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), main);
        assert_eq!(fs::read_to_string(f.config()).unwrap(), original);
    }
}

#[test]
fn undo_preserves_later_unrelated_edits_and_missing_final_newlines() {
    let f = Fixture::new();
    let main = "-- my desktop";
    f.write(&f.main("lua"), main);
    f.write(&f.config(), "# mine\n[tray]\nenabled = false\n");
    f.apply(&f.plan(&["--engine", "echo"]).unwrap());
    let added = "hl.env(\"MY_NEW_SETTING\", \"yes\")\n";
    let updated_main = fs::read_to_string(f.main("lua")).unwrap() + added;
    f.write(&f.main("lua"), &updated_main);
    let updated_config = fs::read_to_string(f.config()).unwrap() + "\n[apps]\nremember = true\n";
    f.write(&f.config(), &updated_config);
    undo(&f.locations, false).unwrap();
    assert_eq!(
        fs::read_to_string(f.main("lua")).unwrap(),
        format!("{main}\n{added}")
    );
    let text = fs::read_to_string(f.config()).unwrap();
    let config = Config::parse(&text).unwrap().0;
    assert!(text.contains("# mine"));
    assert!(!config.tray.enabled);
    assert!(config.apps.remember);
    assert!(config.initial_input_method.is_none());
    assert!(config.engines.is_none());
}

#[test]
fn edits_to_a_managed_file_block_undo_before_any_file_is_changed() {
    let f = Fixture::new();
    let p = f.plan(&["--engine", "echo"]).unwrap();
    f.apply(&p);
    let snippet = f.locations.config_dir.join("hyprland.lua");
    f.write(
        &snippet,
        &(fs::read_to_string(&snippet).unwrap() + "-- manually edited\n"),
    );
    let snapshots: Vec<_> = p
        .record
        .files
        .iter()
        .map(|file| (file.path.clone(), fs::read_to_string(&file.path).unwrap()))
        .collect();
    assert!(undo(&f.locations, false).is_err());
    for (path, text) in snapshots {
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
    assert!(f.locations.journal.exists());
}

#[test]
fn failed_apply_rolls_back_the_files_already_written() {
    let f = Fixture::new();
    f.write(&f.main("lua"), "-- original\n");
    let p = f.plan(&["--engine", "echo"]).unwrap();
    // Another actor creates a directory where the second planned file would go.
    fs::create_dir_all(f.locations.config_dir.join("hyprland.lua")).unwrap();
    assert!(files::apply(&f.locations.journal, None, p.record, &p.changes).is_err());
    assert_eq!(fs::read_to_string(f.main("lua")).unwrap(), "-- original\n");
    assert!(!f.config().exists());
    assert!(!f.locations.journal.exists());
}

#[test]
fn an_interrupted_update_can_be_undone_with_its_journal() {
    for apply_first_change in [false, true] {
        let f = Fixture::new();
        f.apply(&f.plan(&["--engine", "echo"]).unwrap());
        let mut p = f.plan(&["--engine", "sparkle"]).unwrap();
        p.record.complete = false;
        f.write(&f.locations.journal, &toml::to_string(&p.record).unwrap());
        if apply_first_change {
            let first = &p.changes[0];
            f.write(&first.path, first.after.as_deref().unwrap());
        }
        assert!(f.plan(&[]).is_err());
        undo(&f.locations, false).unwrap();
        assert!(!f.main("lua").exists());
        assert!(!f.config().exists());
        assert!(!f.locations.journal.exists());
    }
}

#[test]
fn symlinked_configs_and_compound_startups_are_not_replaced() {
    let f = Fixture::new();
    let target = f.root.join("declarative.lua");
    f.write(&target, "-- managed elsewhere\n");
    fs::create_dir_all(f.main("lua").parent().unwrap()).unwrap();
    symlink(&target, f.main("lua")).unwrap();
    assert!(f.plan(&["--engine", "echo"]).is_err());
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "-- managed elsewhere\n"
    );
    assert!(
        fs::symlink_metadata(f.main("lua"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        configure_hyprland(
            "exec-once = fcitx5 -d && another-app\n",
            Path::new("/tmp/snippet.conf"),
            false,
            true
        )
        .is_err()
    );
}

#[test]
fn desktop_selection_is_explicit_or_matches_the_session() {
    for (current, session, sway_socket, hyprland_socket, expected) in [
        ("sway", "", false, false, Some(Desktop::Sway)),
        ("GNOME", "", false, true, Some(Desktop::Gnome)),
        ("KDE", "", true, false, Some(Desktop::Kde)),
        ("wlroots:SWAY", "", false, true, Some(Desktop::Sway)),
        ("Hyprland", "sway", true, false, Some(Desktop::Hyprland)),
        ("", "sway", false, false, Some(Desktop::Sway)),
        ("", "", true, false, Some(Desktop::Sway)),
        ("", "", false, true, Some(Desktop::Hyprland)),
        ("", "", false, false, None),
    ] {
        let environment = DesktopEnvironment {
            current: current.into(),
            session: session.into(),
            sway_socket,
            hyprland_socket,
        };
        assert_eq!(environment.detect().unwrap(), expected);
    }
    for environment in [
        DesktopEnvironment {
            current: "niri".into(),
            ..Default::default()
        },
        DesktopEnvironment {
            current: "sway:Hyprland".into(),
            ..Default::default()
        },
        DesktopEnvironment {
            sway_socket: true,
            hyprland_socket: true,
            ..Default::default()
        },
    ] {
        assert!(environment.detect().is_err());
        assert_eq!(
            select_desktop(Some(Desktop::Sway), None, &environment).unwrap(),
            Desktop::Sway
        );
    }
    assert!(Options::parse(&["--desktop", "unknown"]).is_err());
    assert!(Options::parse(&["--desktop"]).is_err());
    assert!(Options::parse(&["--undo", "--desktop", "auto"]).is_err());
    assert!(Options::parse(&["--desktop", "sway", "--desktop", "sway"]).is_err());
    assert_eq!(
        Options::parse(&["--desktop", "auto"]).unwrap().desktop,
        None
    );
}

#[test]
fn sway_setup_is_reversible_idempotent_and_remembers_its_desktop() {
    let f = Fixture::new();
    let root = f.locations.config_dir.parent().unwrap();
    let main = root.join("sway/config");
    let uwsm = root.join("uwsm/env-sway");
    let original = "# my desktop\nset $mod Mod4\nexec --no-startup-id fcitx5 -d\noutput * scale 1";
    f.write(&main, original);
    f.write(&uwsm, "export MY_SESSION=1\n");
    assert!(f.plan(&["--desktop", "sway"]).is_err());
    let plan = f
        .plan(&["--desktop", "sway", "--engine", "echo", "--take-over"])
        .unwrap();
    assert_eq!(plan.record.desktop, Desktop::Sway);
    assert_eq!(plan.record.config_path, main);
    assert!(!f.locations.state_dir.exists());
    f.apply(&plan);
    let configured = fs::read_to_string(&main).unwrap();
    assert!(configured.contains("include \""));
    assert!(configured.contains("oh-my-ime/sway.conf\""));
    assert!(configured.contains("# exec --no-startup-id fcitx5 -d"));
    let snippet = fs::read_to_string(f.locations.config_dir.join("sway.conf")).unwrap();
    assert!(snippet.contains("dbus-update-activation-environment --systemd"));
    assert!(snippet.contains("QT_IM_MODULES='wayland;fcitx'"));
    assert!(!snippet.contains("exec_always"));
    assert!(
        fs::read_to_string(&uwsm)
            .unwrap()
            .contains("export XMODIFIERS='@im=fcitx'")
    );
    assert!(!root.join("hypr").exists());
    assert!(!root.join("uwsm/env-hyprland").exists());
    assert!(f.plan(&[]).unwrap().changes.is_empty());
    assert!(f.plan(&["--desktop", "hyprland"]).is_err());
    f.write(&main, &(configured + "bindsym $mod+Return exec foot\n"));
    undo(&f.locations, false).unwrap();
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        format!("{original}\nbindsym $mod+Return exec foot\n")
    );
    assert_eq!(fs::read_to_string(&uwsm).unwrap(), "export MY_SESSION=1\n");
    assert!(!f.locations.config_dir.join("sway.conf").exists());
    assert!(!root.join("environment.d/90-oh-my-ime.conf").exists());
}

#[test]
fn old_journals_remain_hyprland_and_can_be_undone() {
    let f = Fixture::new();
    let plan = f.plan(&["--engine", "echo"]).unwrap();
    f.apply(&plan);
    let mut journal: toml::Value =
        toml::from_str(&fs::read_to_string(&f.locations.journal).unwrap()).unwrap();
    journal.as_table_mut().unwrap().remove("desktop");
    f.write(&f.locations.journal, &toml::to_string(&journal).unwrap());
    assert_eq!(
        Record::load(&f.locations.journal).unwrap().unwrap().desktop,
        Desktop::Hyprland
    );
    assert!(f.plan(&[]).unwrap().changes.is_empty());
    undo(&f.locations, false).unwrap();
    assert!(!f.main("lua").exists());
    assert!(!f.config().exists());
}

#[test]
fn sway_uses_custom_config_paths_and_does_not_replace_symlinks() {
    let f = Fixture::new();
    let main = f.root.join("custom desktop config");
    let plan = f
        .plan(&["--desktop", "sway", "--config", main.to_str().unwrap()])
        .unwrap();
    assert_eq!(plan.record.config_path, main);
    let target = f.root.join("declarative-config");
    f.write(&target, "set $mod Mod4\n");
    symlink(&target, &main).unwrap();
    assert!(
        f.plan(&["--desktop", "sway", "--config", main.to_str().unwrap()])
            .is_err()
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "set $mod Mod4\n");
}

#[test]
fn sway_startup_recognition_preserves_unrelated_commands() {
    for command in [
        "exec fcitx5 -d",
        "exec_always --no-startup-id ibus-daemon -drx",
        "exec \"fcitx5 -d\"",
        "exec env FOO=bar /usr/bin/omimed",
    ] {
        assert!(sway_startup_program(command).unwrap().is_some());
    }
    for command in [
        "# exec fcitx5",
        "bindsym $mod+f exec fcitx5",
        "exec foot",
        "execute fcitx5",
    ] {
        assert!(sway_startup_program(command).unwrap().is_none());
    }
    for command in [
        "exec fcitx5 -d && foot",
        "exec omimed; foot",
        "exec_always ibus-daemon | tee log",
    ] {
        assert!(sway_startup_program(command).is_err());
    }
    for path in [
        "/tmp/$desktop/sway.conf",
        "/tmp/config*/sway.conf",
        "/tmp/`id`/sway.conf",
        "/tmp/quote\"/sway.conf",
        "/tmp/quote'/sway.conf",
        "/tmp/back\\slash/sway.conf",
    ] {
        assert!(configure_sway("", Path::new(path), false).is_err());
    }
    let (text, _) =
        configure_sway("", Path::new("/tmp/config with spaces/sway.conf"), false).unwrap();
    assert!(text.contains("include \"/tmp/config with spaces/sway.conf\""));
}

#[test]
fn native_desktop_registration_is_reversible_and_preserves_session_environment() {
    for (desktop, file) in [
        (
            "kde",
            "applications/io.github.ign1x.OhMyIme.InputMethod.desktop",
        ),
        ("gnome", "ibus/component/oh-my-ime.xml"),
    ] {
        let f = Fixture::new();
        let plan = f.plan(&["--desktop", desktop, "--engine", "echo"]).unwrap();
        f.apply(&plan);
        let registration = f.locations.data_dir.join(file);
        let content = fs::read_to_string(&registration).unwrap();
        assert!(content.contains(f.daemon.to_str().unwrap()));
        assert!(f.plan(&["--desktop", desktop]).unwrap().changes.is_empty());
        assert!(
            !f.locations
                .config_dir
                .parent()
                .unwrap()
                .join("environment.d/90-oh-my-ime.conf")
                .exists()
        );
        let config = f.config();
        f.write(
            &config,
            &(fs::read_to_string(&config).unwrap() + "\n# keep me\n"),
        );
        undo(&f.locations, false).unwrap();
        assert!(!registration.exists());
        assert!(fs::read_to_string(config).unwrap().contains("# keep me"));
    }
}
