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
            state_dir,
        };
        let engines = [("echo", false), ("sparkle", true)].into_iter().map(|(id, enabled)| {
            let text = format!("[engine]\nid = {id:?}\nname = {id:?}\nexec = [{}]\napi = \"1.0\"\nenabled_by_default = {enabled}\n", crate::json_string(daemon.to_str().unwrap()));
            manifest::InstalledEngine { manifest: manifest::Manifest::parse(&text).unwrap().0, dir: root.clone(), unknown_keys: Vec::new() }
        }).collect();
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

#[test]
fn preview_is_read_only_and_enables_the_selected_engine_without_hiding_defaults() {
    let f = Fixture::new();
    let p = f.plan(&["--engine", "echo", "--dry-run"]).unwrap();
    assert_eq!(p.changes.len(), 3);
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
    assert_eq!(parsed.engines, Some(vec!["sparkle".into(), "echo".into()]));
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
