//! Hyprland first-run setup. The plan is built before any files are changed; undo
//! removes owned edits and leaves unrelated user configuration in place.

mod files;
mod session;
#[cfg(test)]
mod tests;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use omime_proto::config::{self, Config};
use omime_proto::{manifest, paths};

use files::{Change, Edit, Kind, ManagedFile, Record};

pub(super) const ENVIRONMENT: [(&str, &str); 4] = [
    ("GTK_IM_MODULE", ""),
    ("QT_IM_MODULE", "fcitx"),
    ("QT_IM_MODULES", "wayland;fcitx"),
    ("XMODIFIERS", "@im=fcitx"),
];

#[derive(Debug, Default)]
pub struct Options {
    engine: Option<String>,
    config: Option<PathBuf>,
    dry_run: bool,
    undo: bool,
    start: bool,
    take_over: bool,
}

impl Options {
    pub fn parse(args: &[&str]) -> anyhow::Result<Self> {
        let mut options = Self::default();
        let mut args = args.iter().copied();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(arg) = args.next() {
            ensure!(seen.insert(arg), "duplicate setup option {arg}");
            match arg {
                "--engine" => {
                    let id = args.next().context("--engine needs an input method id")?;
                    ensure!(
                        config::valid_input_method(id),
                        "invalid input method id `{id}`"
                    );
                    options.engine = Some(id.into());
                }
                "--config" => {
                    options.config = Some(
                        args.next()
                            .context("--config needs a Hyprland configuration path")?
                            .into(),
                    )
                }
                "--dry-run" => options.dry_run = true,
                "--undo" => options.undo = true,
                "--start" => options.start = true,
                "--take-over" => options.take_over = true,
                _ => bail!("unknown setup option `{arg}`"),
            }
        }
        ensure!(
            !options.undo
                || (options.engine.is_none()
                    && options.config.is_none()
                    && !options.start
                    && !options.take_over),
            "setup --undo accepts only --dry-run"
        );
        Ok(options)
    }
}

#[derive(Debug)]
struct Locations {
    config_dir: PathBuf,
    state_dir: PathBuf,
    journal: PathBuf,
}

impl Locations {
    fn from_environment() -> anyhow::Result<Self> {
        let config_dir = std::path::absolute(
            paths::config_dir().context("cannot find XDG_CONFIG_HOME or HOME")?,
        )?;
        let state_dir =
            std::path::absolute(paths::state_dir().context("cannot find XDG_STATE_HOME or HOME")?)?;
        Ok(Self {
            journal: state_dir.join("setup.toml"),
            config_dir,
            state_dir,
        })
    }
}

#[derive(Debug)]
struct Plan {
    record: Record,
    previous: Option<Record>,
    changes: Vec<Change>,
    daemon: PathBuf,
}

pub async fn run(options: Options) -> anyhow::Result<()> {
    let locations = Locations::from_environment()?;
    // Preview never creates a lock, journal, configuration or state directory.
    let _lock = if options.dry_run {
        None
    } else {
        fs::create_dir_all(&locations.state_dir)?;
        let path = locations.state_dir.join("setup.lock");
        let _ = files::read(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .context("another omime setup is running")?;
        Some(file)
    };
    if options.undo {
        return undo(&locations, options.dry_run);
    }

    let plan = prepare(&locations, &options)?;
    let runtime = if options.start && !options.dry_run {
        Some(session::prepare(options.take_over).await?)
    } else {
        None
    };
    print_changes(&plan.changes, options.dry_run);
    if let Some(engine) = &plan.record.engine {
        println!("First-login input method: {engine}.");
    }
    if plan.record.take_over {
        println!(
            "Recognized Fcitx5/IBus autostart commands in the selected Hyprland file will be suspended."
        );
    }
    if options.dry_run {
        if options.start {
            println!("Start omimed and select the requested input method after configuring.");
        }
        return Ok(());
    }
    if plan.previous.as_ref() != Some(&plan.record) {
        files::apply(
            &locations.journal,
            plan.previous.as_ref(),
            plan.record.clone(),
            &plan.changes,
        )?;
    }
    println!("Hyprland setup is ready. Use omime setup --undo to restore the managed settings.");
    if let Some(runtime) = runtime {
        session::start(runtime, &plan.daemon, &locations.state_dir, options.engine.as_deref()).await
            .context("settings were saved, but input could not be started; correct the reported problem and retry omime setup --start")?;
    }
    println!(
        "Session configuration is ready for Hyprland, UWSM and systemd application launches. Log out and back in for every launcher to inherit it, then run omime doctor."
    );
    println!(
        "Qt applications need their Fcitx5 input module (fcitx5-qt); see docs/hyprland.md for application requirements."
    );
    Ok(())
}

fn prepare(locations: &Locations, options: &Options) -> anyhow::Result<Plan> {
    let found = manifest::discover(&paths::engine_dirs());
    prepare_with(locations, options, find_daemon()?, &found.engines)
}

fn prepare_with(
    locations: &Locations,
    options: &Options,
    daemon: PathBuf,
    installed: &[manifest::InstalledEngine],
) -> anyhow::Result<Plan> {
    let previous = Record::load(&locations.journal)?;
    ensure!(
        previous.as_ref().is_none_or(|r| r.complete),
        "an earlier setup was interrupted; run omime setup --undo to recover it before applying again"
    );
    let requested = options
        .config
        .as_ref()
        .map(std::path::absolute)
        .transpose()?;
    if let (Some(old), Some(path)) = (&previous, &requested) {
        ensure!(
            old.config_path == *path,
            "setup already manages {}; undo it before choosing another config",
            old.config_path.display()
        );
    }
    let config_path = requested
        .or_else(|| previous.as_ref().map(|p| p.config_path.clone()))
        .unwrap_or_else(|| {
            let hypr = locations
                .config_dir
                .parent()
                .unwrap_or(&locations.config_dir)
                .join("hypr");
            let lua = hypr.join("hyprland.lua");
            let legacy = hypr.join("hyprland.conf");
            if lua.exists() || !legacy.exists() {
                lua
            } else {
                legacy
            }
        });
    let lua = match config_path.extension().and_then(|e| e.to_str()) {
        Some("lua") => true,
        Some("conf") => false,
        _ => bail!("--config must name a .lua or .conf Hyprland configuration"),
    };
    let snippet = locations
        .config_dir
        .join(if lua { "hyprland.lua" } else { "hyprland.conf" });
    let user_config = locations.config_dir.join(config::CONFIG_FILE);
    // A setup that keeps the current method still needs a usable configuration.
    if let Some(text) = files::read(&user_config)? {
        Config::parse(&text).map_err(anyhow::Error::msg)?;
    }
    ensure!(
        config_path != snippet && config_path != user_config,
        "--config must name Hyprland's main configuration, not an oh-my-ime file"
    );
    let engine = options
        .engine
        .clone()
        .or_else(|| previous.as_ref().and_then(|p| p.engine.clone()));
    if let Some(target) = engine.as_deref().filter(|e| *e != "direct") {
        let id = target.split('/').next().unwrap_or(target);
        let installed = installed.iter().find(|e| e.id() == id)
            .with_context(|| format!("input method engine `{id}` is not installed or its required program is unavailable (see omime doctor)"))?;
        if let Some(problem) = crate::install::command_warning(installed) {
            bail!("{problem}");
        }
    }
    let take_over = options.take_over || previous.as_ref().is_some_and(|p| p.take_over);
    let mut record = Record {
        version: 1,
        complete: true,
        config_path: config_path.clone(),
        engine: engine.clone(),
        take_over,
        files: Vec::new(),
    };
    let mut changes = Vec::new();
    let base = |path: &Path| -> anyhow::Result<(Option<String>, Option<String>)> {
        let current = files::read(path)?;
        let original = match previous
            .as_ref()
            .and_then(|p| p.files.iter().find(|f| f.path == path))
        {
            Some(file) => files::unmanage(file, current.as_deref())?,
            None => current.clone(),
        };
        Ok((current, original))
    };

    let (current, original) = base(&config_path)?;
    let (text, edits) =
        configure_hyprland(original.as_deref().unwrap_or(""), &snippet, lua, take_over)?;
    add_file(
        &mut record,
        &mut changes,
        config_path,
        current,
        original,
        text,
        Kind::Text { edits },
    );

    let (current, original) = base(&snippet)?;
    ensure!(
        original.is_none(),
        "{} already exists outside setup; keep it and choose another configuration location",
        snippet.display()
    );
    let content = snippet_text(&daemon, lua)?;
    add_file(
        &mut record,
        &mut changes,
        snippet,
        current,
        original,
        content,
        Kind::Whole,
    );

    // Applications launched by the user manager or UWSM do not inherit Hyprland's
    // environment. Configure their launch environments as part of the same journal.
    let config_root = locations
        .config_dir
        .parent()
        .context("no configuration directory")?;
    let systemd_env = config_root.join("environment.d/90-oh-my-ime.conf");
    let (current, original) = base(&systemd_env)?;
    ensure!(
        original.is_none(),
        "{} already exists outside setup",
        systemd_env.display()
    );
    let mut content = String::from("# Managed by omime setup.\n");
    for (key, value) in ENVIRONMENT {
        content += &format!("{key}={value}\n");
    }
    add_file(
        &mut record,
        &mut changes,
        systemd_env,
        current,
        original,
        content,
        Kind::Whole,
    );

    let uwsm_env = config_root.join("uwsm/env-hyprland");
    let (current, original) = base(&uwsm_env)?;
    let mut content = original.clone().unwrap_or_default();
    ensure!(
        !content.contains("omime setup begin"),
        "{} has a setup block without its journal",
        uwsm_env.display()
    );
    let mut block = if content.is_empty() || content.ends_with('\n') {
        String::new()
    } else {
        "\n".into()
    };
    block += "# omime setup begin\n";
    for (key, value) in ENVIRONMENT {
        block += &format!("export {key}={}\n", shell_quote(value));
    }
    block += "# omime setup end\n";
    content += &block;
    add_file(
        &mut record,
        &mut changes,
        uwsm_env,
        current,
        original,
        content,
        Kind::Text {
            edits: vec![Edit {
                before: String::new(),
                after: block,
            }],
        },
    );

    if let Some(target) = engine {
        let (current, original) = base(&user_config)?;
        let text = original.as_deref().unwrap_or("");
        let (config, _) = Config::parse(text).map_err(anyhow::Error::msg)?;
        let mut edited = config::edit(
            text,
            "initial_input_method",
            config::Value::String(target.clone()),
        )
        .map_err(anyhow::Error::msg)?;
        let mut keys = vec!["initial_input_method".to_owned()];
        let id = target.split('/').next().unwrap_or(&target);
        if id != "direct" {
            let enabled = config.engines.clone().unwrap_or_else(|| {
                installed
                    .iter()
                    .filter(|e| e.manifest.engine.enabled_by_default)
                    .map(|e| e.id().to_owned())
                    .collect()
            });
            if !enabled.iter().any(|e| e == id || e == &target) {
                let mut enabled = enabled;
                enabled.push(target);
                edited = config::edit(&edited, "engines", config::Value::List(enabled))
                    .map_err(anyhow::Error::msg)?;
                keys.push("engines".into());
            }
        }
        add_file(
            &mut record,
            &mut changes,
            user_config,
            current,
            original,
            edited,
            Kind::Config { keys },
        );
    }
    Ok(Plan {
        record,
        previous,
        changes,
        daemon,
    })
}

fn add_file(
    record: &mut Record,
    changes: &mut Vec<Change>,
    path: PathBuf,
    current: Option<String>,
    original: Option<String>,
    installed: String,
    change: Kind,
) {
    let mode = files::mode(&path);
    if current.as_deref() != Some(installed.as_str()) {
        changes.push(Change {
            path: path.clone(),
            before: current.clone(),
            after: Some(installed.clone()),
            mode,
        });
    }
    record.files.push(ManagedFile {
        path,
        original,
        previous: current,
        installed,
        mode,
        change,
    });
}

fn undo(locations: &Locations, dry_run: bool) -> anyhow::Result<()> {
    let Some(record) = Record::load(&locations.journal)? else {
        println!("No setup to undo.");
        return Ok(());
    };
    let mut changes = Vec::new();
    // Resolve every conflict before modifying any of the files.
    for file in record.files.iter().rev() {
        let current = files::read(&file.path)?;
        let restored = if !record.complete && current == file.previous {
            file.original.clone()
        } else {
            files::unmanage(file, current.as_deref())?
        };
        if restored != current {
            changes.push(Change {
                path: file.path.clone(),
                before: current,
                after: restored,
                mode: file.mode,
            });
        }
    }
    print_changes(&changes, dry_run);
    if !dry_run {
        files::undo(&locations.journal, &changes)?;
        println!(
            "Managed setup removed. Log out and back in to restore the previous session startup and input environment."
        );
    }
    Ok(())
}

fn print_changes(changes: &[Change], dry_run: bool) {
    if changes.is_empty() {
        println!("Configuration is already up to date.");
    }
    for change in changes {
        let action = if change.after.is_none() {
            "Remove"
        } else if change.before.is_none() {
            "Create"
        } else {
            "Update"
        };
        println!(
            "{}{action} {}",
            if dry_run { "Would " } else { "" },
            change.path.display()
        );
    }
}

fn find_daemon() -> anyhow::Result<PathBuf> {
    if let Some(path) = crate::install::find_in_path("omimed") {
        return Ok(std::path::absolute(path)?);
    }
    let path = std::env::current_exe()?
        .parent()
        .context("cannot locate omime")?
        .join("omimed");
    ensure!(
        path.is_file(),
        "omimed is not installed; install the platform before setup"
    );
    Ok(path)
}

fn path_text(path: &Path) -> anyhow::Result<&str> {
    let text = path.to_str().context("configuration paths must be UTF-8")?;
    ensure!(
        !text.chars().any(char::is_control),
        "configuration paths must not contain control characters"
    );
    Ok(text)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn snippet_text(daemon: &Path, lua: bool) -> anyhow::Result<String> {
    let command = shell_quote(path_text(daemon)?);
    let mut text = if lua {
        "-- Managed by omime setup.\n"
    } else {
        "# Managed by omime setup.\n"
    }
    .to_owned();
    for (key, value) in ENVIRONMENT {
        if lua {
            text += &format!(
                "hl.env({}, {})\n",
                crate::json_string(key),
                crate::json_string(value)
            );
        } else {
            text += &format!("env = {key},{value}\n");
        }
    }
    if lua {
        text += &format!(
            "hl.on(\"hyprland.start\", function()\n    hl.exec_cmd({})\nend)\n",
            crate::json_string(&command)
        );
    } else {
        text += &format!("exec-once = {command}\n");
    }
    Ok(text)
}

fn configure_hyprland(
    original: &str,
    snippet: &Path,
    lua: bool,
    take_over: bool,
) -> anyhow::Result<(String, Vec<Edit>)> {
    ensure!(
        !original.contains("omime setup begin"),
        "Hyprland contains an omime setup block without its journal; restore the journal before proceeding"
    );
    let comment = if lua { "--" } else { "#" };
    let mut text = String::new();
    let mut edits = Vec::new();
    for line in original.split_inclusive('\n') {
        if let Some(program) = startup_program(line, lua)? {
            ensure!(
                program == "omimed" || take_over,
                "Hyprland already starts {program}; use --take-over to suspend that startup, or remove it yourself before setup"
            );
            let after = format!(
                "{comment} omime setup disabled startup {}\n{comment} {}\n",
                edits.len(),
                line.trim_end_matches(['\r', '\n'])
            );
            text += &after;
            edits.push(Edit {
                before: line.into(),
                after,
            });
        } else {
            text += line;
        }
    }
    let source = path_text(snippet)?;
    let include = if lua {
        format!("dofile({})", crate::json_string(source))
    } else {
        ensure!(
            !source.contains(['$', '#', '*', '?', '[', ']']),
            "this path cannot be represented as a literal Hyprland .conf include; use a .lua configuration"
        );
        format!("source = {source}")
    };
    let block = format!(
        "{}{comment} omime setup begin\n{include}\n{comment} omime setup end\n",
        if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        }
    );
    text += &block;
    edits.push(Edit {
        before: String::new(),
        after: block,
    });
    Ok((text, edits))
}

/// Only rewrite standalone launches we understand. Shell pipelines and arbitrary
/// Lua expressions retain their original meaning and must be configured by their author.
fn startup_program(line: &str, lua: bool) -> anyhow::Result<Option<&str>> {
    let line = line.trim();
    let command = if lua {
        let Some(call) = line.strip_prefix("hl.exec_cmd(") else {
            return Ok(None);
        };
        let Some(quote) = call.chars().next() else {
            return Ok(None);
        };
        if !matches!(quote, '\'' | '"') {
            return Ok(None);
        }
        let rest = &call[1..];
        let Some(end) = rest.find(quote) else {
            return Ok(None);
        };
        if rest[end + 1..].trim() != ")" {
            return Ok(None);
        }
        &rest[..end]
    } else {
        let Some((key, value)) = line.split_once('=') else {
            return Ok(None);
        };
        if key.trim() != "exec-once" {
            return Ok(None);
        }
        value.trim()
    };
    let mut words = command.split_whitespace();
    let Some(mut program) = words.next() else {
        return Ok(None);
    };
    if program == "env" {
        let Some(next) = words.find(|word| !word.contains('=')) else {
            return Ok(None);
        };
        program = next;
    }
    let name = program
        .trim_matches(['\'', '"'])
        .rsplit('/')
        .next()
        .unwrap_or(program);
    if !matches!(name, "omimed" | "fcitx5" | "fcitx" | "ibus-daemon") {
        return Ok(None);
    }
    ensure!(
        !command.contains([';', '|', '&', '<', '>', '`', '$', '\\']),
        "a compound {name} startup also performs other actions; split that startup in Hyprland before using setup"
    );
    Ok(Some(name))
}
