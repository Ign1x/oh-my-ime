//! `omime` — manage the oh-my-ime input method platform.

mod doctor;
mod install;
mod setup;
mod watch;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use omime_proto::names::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};
use zbus::OwnedVariant;

const USAGE: &str = "\
Usage: omime <COMMAND>

Commands:
  list [--all]     List the input methods to choose from (* marks the one in use);
                   with --all also those you can enable (marked -)
  engines          List installed engines (* marks enabled ones)
  status [--json] [--watch]
                   Show the input method in use (`--watch` keeps streaming changes)
  switch <ID>      Switch to an input method from `list`, such as `sparkle` or
                   `fcitx5/shuangpin` (`direct` for no input method)
  next             Switch to the next input method
  toggle           Switch between direct input and the last engine
  enable <ENGINE>  Enable an engine (adds it to `engines` in config.toml)
  disable <ENGINE> Disable an engine (removes it from `engines` in config.toml)
  settings [<ENGINE>]
                   Open oh-my-ime's settings, or those of an engine, such as `fcitx5`
  properties       Show the menu of the input method in use (switches such as
                   Chinese or English punctuation)
  activate <ID>    Activate an entry of that menu
  apps [forget <APP>]
                   Show the input method each application gets back with
                   `[apps] remember`, or forget one
  config [<KEY> <VALUE>]
                   Show the options of config.toml, or set one, such as
                   `omime config apps.remember true`
  install <DIR>    Install an engine from a directory (`--force` to replace it)
  uninstall <ID>   Remove an engine installed in your user directory
  reload           Re-read config.toml and look for installed or removed engines
  doctor           Explain what is wrong with the setup (fails on problems)
  setup [--engine ID] [--config PATH] [--dry-run] [--start] [--take-over]
                   Configure Hyprland; --start also starts input in this session
  setup --undo [--dry-run]
                   Undo the managed setup, preserving other configuration changes
  help             Print this help

The other commands talk to a running `omimed` over the session bus; `install`,
`uninstall`, `setup` and `doctor` also work without it, `status --watch` keeps watching
for it, and the settings wait for it.";

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Daemon1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait Daemon {
    fn list_engines(&self) -> zbus::Result<Vec<(String, String, bool, String)>>;
    fn list_input_methods(&self) -> zbus::Result<Vec<(String, String, String, String, bool)>>;
    fn status(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    fn switch(&self, engine: &str) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
    fn reload(&self) -> zbus::Result<()>;
    fn open_settings(&self, engine: &str) -> zbus::Result<()>;
    fn input_methods(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
    fn config(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    fn apps(&self) -> zbus::Result<Vec<(String, String)>>;
    fn properties(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
    fn activate_property(&self, id: &str) -> zbus::Result<()>;
    fn forget_app(&self, app: &str) -> zbus::Result<()>;
    fn configure(&self, changes: HashMap<&str, zbus::Variant<'_>>) -> zbus::Result<()>;

    /// The compositor, the seats and whether focus is reported; see `doctor`.
    fn diagnose(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;

    /// Emitted when what `Status` returns changes. Carries the new status.
    #[zbus(signal)]
    fn status_changed(&self, status: HashMap<String, OwnedVariant>) -> zbus::Result<()>;
}

enum Command {
    /// Commands that need a running omimed.
    Daemon(DaemonCommand),
    Install {
        dir: PathBuf,
        force: bool,
    },
    Uninstall(String),
    /// `status --watch`, which keeps running while omimed is away.
    Watch {
        json: bool,
    },
    /// Checks the setup; needs neither omimed nor a session bus.
    Doctor,
    /// Opens oh-my-ime's settings, `omime-settings`, which waits for omimed itself.
    OwnSettings,
    Setup(setup::Options),
}

enum DaemonCommand {
    List,
    ListAll,
    Engines,
    Status {
        json: bool,
    },
    Switch(String),
    Next,
    Toggle,
    Enable(String),
    Disable(String),
    Reload,
    Settings(String),
    /// Shows the configuration (`None`) or sets an option.
    Config(Option<(String, String)>),
    Apps,
    ForgetApp(String),
    Properties,
    Activate(String),
}

fn parse(args: &[String]) -> anyhow::Result<Option<Command>> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(Some(match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => return Ok(None),
        ["setup", "-h" | "--help"] => return Ok(None),
        ["install", dir] if *dir != "--force" => Command::Install {
            dir: PathBuf::from(*dir),
            force: false,
        },
        ["install", dir, "--force"] | ["install", "--force", dir] => Command::Install {
            dir: PathBuf::from(*dir),
            force: true,
        },
        ["uninstall", engine] => Command::Uninstall((*engine).to_owned()),
        ["doctor"] => Command::Doctor,
        ["settings"] => Command::OwnSettings,
        ["setup", rest @ ..] => Command::Setup(setup::Options::parse(rest)?),
        ["status", "--watch"] => Command::Watch { json: false },
        ["status", "--json", "--watch"] | ["status", "--watch", "--json"] => {
            Command::Watch { json: true }
        }
        args => Command::Daemon(match args {
            ["list"] => DaemonCommand::List,
            ["list", "--all"] => DaemonCommand::ListAll,
            ["engines"] => DaemonCommand::Engines,
            ["status"] => DaemonCommand::Status { json: false },
            ["status", "--json"] => DaemonCommand::Status { json: true },
            ["switch", engine] => DaemonCommand::Switch((*engine).to_owned()),
            ["next"] => DaemonCommand::Next,
            ["toggle"] => DaemonCommand::Toggle,
            ["enable", engine] => DaemonCommand::Enable((*engine).to_owned()),
            ["disable", engine] => DaemonCommand::Disable((*engine).to_owned()),
            ["reload"] => DaemonCommand::Reload,
            ["settings", engine] => DaemonCommand::Settings((*engine).to_owned()),
            ["properties"] => DaemonCommand::Properties,
            ["activate", id] => DaemonCommand::Activate((*id).to_owned()),
            ["apps"] => DaemonCommand::Apps,
            ["apps", "forget", app] => DaemonCommand::ForgetApp((*app).to_owned()),
            ["config"] => DaemonCommand::Config(None),
            ["config", key, value] => {
                DaemonCommand::Config(Some(((*key).to_owned(), (*value).to_owned())))
            }
            _ => bail!("unknown command line `{}`\n\n{USAGE}", args.join(" ")),
        }),
    }))
}

/// `text` padded to `width` columns; CJK characters take two.
fn pad(text: &str, width: usize) -> String {
    let columns: usize = text
        .chars()
        .map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 })
        .sum();
    format!("{text}{}", " ".repeat(width.saturating_sub(columns)))
}

fn string(dict: &HashMap<String, OwnedVariant>, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| !s.is_empty())
}

/// Minimal JSON string escaping, enough for ids and labels.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Formats a status dictionary (the reply of `Status` or the argument of
/// `StatusChanged`) as the line `omime status` prints.
fn format_status(status: &HashMap<String, OwnedVariant>, json: bool) -> String {
    let engine = string(status, "engine");
    let method = string(status, "input-method").or_else(|| engine.clone());
    let label = string(status, "label");
    if json {
        let field = |v: &Option<String>| v.as_deref().map_or("null".into(), json_string);
        let tooltip = string(status, "tooltip");
        // `text`, `tooltip` and `class` make this usable as a waybar module.
        let text = label
            .clone()
            .or_else(|| engine.clone())
            .unwrap_or_else(|| "⌨".into());
        let class = engine.clone().unwrap_or_else(|| "direct".into());
        format!(
            "{{\"engine\":{},\"input_method\":{},\"label\":{},\"tooltip\":{},\"text\":{},\"class\":{}}}",
            field(&engine),
            field(&method),
            field(&label),
            field(&tooltip),
            json_string(&text),
            json_string(&class),
        )
    } else {
        match (method, label) {
            (None, _) => "direct input".into(),
            (Some(method), Some(label)) => format!("{method} ({label})"),
            (Some(method), None) => method,
        }
    }
}

/// The line for "omimed is not running", in the same formats as [`format_status`].
fn format_stopped(json: bool) -> String {
    if json {
        // `class` is `stopped`, so status bars can tell it from direct input.
        "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":\"omimed is not running\",\
         \"text\":\"⌨\",\"class\":\"stopped\"}"
            .into()
    } else {
        "omimed is not running".into()
    }
}

async fn run(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Install { dir, force } => install_engine(&dir, force).await?,
        Command::Uninstall(engine) => uninstall_engine(&engine).await?,
        Command::Watch { json } => watch::run(json).await?,
        Command::Setup(options) => setup::run(options).await?,
        Command::Daemon(command) => run_daemon(command).await?,
        Command::OwnSettings => return Err(open_own_settings()),
        Command::Doctor => {
            let clean = doctor::run().await?;
            return Ok(if clean {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `omime-settings` in place of this process: the one next to `omime`, as
/// installed, or else the one in `$PATH`. Returns only when that fails.
fn open_own_settings() -> anyhow::Error {
    use std::os::unix::process::CommandExt as _;
    const PROGRAM: &str = "omime-settings";
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(PROGRAM)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(PROGRAM));
    let error = std::process::Command::new(&program).exec();
    anyhow::Error::new(error).context(format!("cannot run {}", program.display()))
}

/// Installs the engine directory `dir` for the user and lets a running omimed notice.
async fn install_engine(dir: &Path, force: bool) -> anyhow::Result<()> {
    let engines_dir =
        omime_proto::paths::user_engine_dir().context("cannot find the data directory")?;
    let engine = install::install(dir, &engines_dir, force)?;
    if let Some(warning) = install::command_warning(&engine) {
        eprintln!("warning: {warning}");
    }
    for key in &engine.unknown_keys {
        eprintln!("warning: omimed does not know `{key}` in the manifest and ignores it");
    }
    edit_config_engines(engine.id(), true)?;
    let running = reload_if_running().await?;
    println!(
        "Installed {} ({}) into {}",
        engine.id(),
        engine.manifest.engine.name,
        engine.dir.display()
    );
    if !running {
        println!("omimed is not running; it will find the engine when it starts.");
    }
    Ok(())
}

/// Removes the user-installed engine `engine` and lets a running omimed notice.
async fn uninstall_engine(engine: &str) -> anyhow::Result<()> {
    let engines_dir =
        omime_proto::paths::user_engine_dir().context("cannot find the data directory")?;
    install::uninstall(&engines_dir, engine, &omime_proto::paths::engine_dirs())?;
    edit_config_engines(engine, false)?;
    reload_if_running().await?;
    println!("Uninstalled {engine}");
    Ok(())
}

/// Asks a running omimed to reload. Returns `false` when there is none, so installing
/// and uninstalling also work in a session without a bus.
async fn reload_if_running() -> anyhow::Result<bool> {
    let Ok(conn) = zbus::Connection::session().await else {
        return Ok(false);
    };
    if !daemon_owns_bus_name(&conn).await? {
        return Ok(false);
    }
    let daemon = DaemonProxy::new(&conn).await?;
    // The engines are looked up again even when config.toml is broken.
    if let Err(e) = daemon.reload().await {
        eprintln!(
            "warning: omimed kept its configuration: {}",
            dbus_message(&e)
        );
    }
    Ok(true)
}

/// The message of a D-Bus error reply, or the error itself.
fn dbus_message(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(detail), _) => detail.clone(),
        e => e.to_string(),
    }
}

/// Whether omimed currently owns its bus name.
async fn daemon_owns_bus_name(conn: &zbus::Connection) -> anyhow::Result<bool> {
    Ok(conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &DAEMON_BUS_NAME,
        )
        .await?
        .body()
        .deserialize::<bool>()?)
}

/// Adds `engine` to or removes it from an explicit `engines` list in config.toml.
/// Without a list every installed engine is enabled, so there is nothing to change.
fn edit_config_engines(engine: &str, enable: bool) -> anyhow::Result<()> {
    let dir =
        omime_proto::paths::config_dir().context("cannot find the configuration directory")?;
    let path = dir.join(omime_proto::config::CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let updated = edit_engines(&text, engine, enable)
        .with_context(|| format!("cannot update {}", path.display()))?;
    if updated != text {
        // Through a temporary file: omimed may be reading it.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, updated).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

async fn run_daemon(command: DaemonCommand) -> anyhow::Result<()> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let daemon = DaemonProxy::new(&conn).await?;
    if !daemon_owns_bus_name(&conn).await? {
        bail!("omimed is not running (nobody owns {DAEMON_BUS_NAME} at {DAEMON_OBJECT_PATH})");
    }

    match command {
        DaemonCommand::List => {
            for (id, name, engine, _label, in_use) in daemon.list_input_methods().await? {
                let mark = if in_use { '*' } else { ' ' };
                let engine = if engine == name {
                    String::new()
                } else {
                    engine
                };
                println!("{mark} {id:<24} {} {engine}", pad(&name, 24));
            }
        }
        DaemonCommand::ListAll => {
            let flag = |method: &HashMap<String, OwnedVariant>, key: &str| {
                method
                    .get(key)
                    .and_then(|v| v.try_clone().ok())
                    .and_then(|v| bool::try_from(v).ok())
                    .unwrap_or(false)
            };
            for method in daemon.input_methods().await? {
                let mark = match (flag(&method, "in-use"), flag(&method, "enabled")) {
                    (true, _) => '*',
                    (false, true) => ' ',
                    (false, false) => '-',
                };
                let id = string(&method, "id").unwrap_or_default();
                let name = string(&method, "name").unwrap_or_default();
                let engine = string(&method, "engine-name")
                    .filter(|engine| *engine != name)
                    .unwrap_or_default();
                println!("{mark} {id:<24} {} {engine}", pad(&name, 24));
            }
        }
        DaemonCommand::Engines => {
            for (id, name, enabled, state) in daemon.list_engines().await? {
                let mark = if enabled { '*' } else { ' ' };
                println!("{mark} {id:<16} {name:<24} {state}");
            }
        }
        DaemonCommand::Status { json } => {
            let status = daemon.status().await?;
            println!("{}", format_status(&status, json));
        }
        DaemonCommand::Switch(engine) => {
            let target = if engine == "direct" {
                ""
            } else {
                engine.as_str()
            };
            daemon.switch(target).await?;
        }
        DaemonCommand::Next => daemon.next().await?,
        DaemonCommand::Toggle => daemon.toggle().await?,
        DaemonCommand::Enable(engine) => set_enabled(&daemon, &engine, true).await?,
        DaemonCommand::Disable(engine) => set_enabled(&daemon, &engine, false).await?,
        DaemonCommand::Reload => daemon.reload().await?,
        DaemonCommand::Settings(engine) => daemon.open_settings(&engine).await?,
        DaemonCommand::Apps => {
            for (app, target) in daemon.apps().await? {
                let target = if target.is_empty() {
                    "direct"
                } else {
                    target.as_str()
                };
                println!("{app:<32} {target}");
            }
        }
        DaemonCommand::ForgetApp(app) => daemon.forget_app(&app).await?,
        DaemonCommand::Properties => {
            let properties = daemon.properties().await?;
            for property in &properties {
                print!("{}", property_line(property, &properties));
            }
        }
        DaemonCommand::Activate(id) => daemon.activate_property(&id).await?,
        DaemonCommand::Config(None) => {
            let config = daemon.config().await?;
            let mut keys: Vec<&String> = config.keys().collect();
            keys.sort();
            for key in keys {
                println!("{key} = {}", show_value(&config[key]));
            }
        }
        DaemonCommand::Config(Some((key, value))) => {
            let value = parse_value(&key, &value)?;
            daemon
                .configure(HashMap::from([(key.as_str(), value)]))
                .await?;
        }
    }
    Ok(())
}

/// Adds `entry`, an engine or an input method inside one (`fcitx5/rime`), to
/// `engines` in config.toml or removes it (keeping the rest of the file as it is), then
/// asks the daemon to reload.
async fn set_enabled(daemon: &DaemonProxy<'_>, entry: &str, enable: bool) -> anyhow::Result<()> {
    let installed = daemon.list_engines().await?;
    let (engine, inner) = entry
        .split_once('/')
        .map_or((entry, None), |(e, i)| (e, Some(i)));
    if !installed.iter().any(|(id, ..)| id == engine) {
        bail!("no engine `{engine}` is installed (see `omime engines`)");
    }
    // The input methods inside the engine now, which stay when one of them goes.
    let prefix = format!("{engine}/");
    let inside: Vec<String> = daemon
        .list_input_methods()
        .await?
        .into_iter()
        .map(|(id, ..)| id)
        .filter(|id| id.starts_with(&prefix))
        .collect();
    if enable && inner.is_some() && !inside.iter().any(|id| id == entry) {
        eprintln!(
            "note: {engine} does not list `{}` yet; it gets it when you switch to it",
            inner.unwrap_or_default()
        );
    }
    // omimed writes config.toml, so that it is the only one that does.
    let config = daemon.config().await?;
    let list: Vec<String> = match config.get("engines") {
        Some(value) => value.try_clone()?.try_into()?,
        None => installed
            .iter()
            .filter(|(.., enabled, _)| *enabled)
            .map(|(id, ..)| id.clone())
            .collect(),
    };
    let list = edited_entries(list, entry, enable, &inside);
    let value = zbus::Variant::from(list);
    daemon
        .configure(HashMap::from([("engines", value)]))
        .await?;
    Ok(())
}

/// A line of `omime properties`: indented under its menu, with the state of switches.
fn property_line(
    property: &HashMap<String, OwnedVariant>,
    all: &[HashMap<String, OwnedVariant>],
) -> String {
    let text = |p: &HashMap<String, OwnedVariant>, key: &str| string(p, key).unwrap_or_default();
    let mut depth = 0;
    let mut parent = text(property, "parent");
    while !parent.is_empty() && depth < 8 {
        depth += 1;
        parent = all
            .iter()
            .find(|p| text(p, "id") == parent)
            .map(|p| text(p, "parent"))
            .unwrap_or_default();
    }
    let checked = property
        .get("checked")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);
    let mark = match (text(property, "kind").as_str(), checked) {
        ("separator", _) => return format!("{}-\n", "  ".repeat(depth)),
        ("toggle" | "radio", true) => "[x] ",
        ("toggle" | "radio", false) => "[ ] ",
        _ => "",
    };
    format!(
        "{}{mark}{}  ({})\n",
        "  ".repeat(depth),
        text(property, "label"),
        text(property, "id")
    )
}

/// An option's value as config.toml writes it.
fn show_value(value: &OwnedVariant) -> String {
    use zbus::Variant as Value;
    match &**value {
        Value::Str(s) => format!("{:?}", s.as_str()),
        Value::Bool(b) => b.to_string(),
        Value::U64(n) => n.to_string(),
        Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|item| match item {
                    Value::Str(s) => format!("{:?}", s.as_str()),
                    other => format!("{other}"),
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        other => format!("{other}"),
    }
}

/// The value `text` gives the option `key` on the command line: true or false, a
/// number, or a list of comma-separated entries for `engines`.
fn parse_value(key: &str, text: &str) -> anyhow::Result<zbus::Variant<'static>> {
    use zbus::Variant as Value;
    Ok(match key {
        "apps.remember" | "tray.enabled" | "panel.enabled" | "notifications.enabled" => {
            Value::from(
                text.parse::<bool>()
                    .with_context(|| format!("`{key}` is true or false"))?,
            )
        }
        "engine.key_timeout_ms" => Value::from(
            text.parse::<u64>()
                .with_context(|| format!("`{key}` is a number of milliseconds"))?,
        ),
        "engines" => Value::from(
            text.split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
                .collect::<Vec<String>>(),
        ),
        _ => Value::from(text.to_owned()),
    })
}

/// `list`, the `engines` of config.toml, with `entry` (an engine, or an input method
/// inside one) added or removed. Removing an input method of an engine listed as a
/// whole lists the engine's other input methods, `inside`, instead; removing an engine
/// removes its input methods too.
fn edited_entries(
    mut list: Vec<String>,
    entry: &str,
    enable: bool,
    inside: &[String],
) -> Vec<String> {
    match (enable, entry.split_once('/')) {
        (true, _) => {
            if !list.iter().any(|e| e == entry) {
                list.push(entry.to_owned());
            }
        }
        (false, Some((engine, _))) => {
            if let Some(at) = list.iter().position(|e| e == engine) {
                let others = inside.iter().filter(|m| *m != entry).cloned();
                list.splice(at..=at, others);
            }
            list.retain(|e| e != entry);
        }
        (false, None) => {
            let prefix = format!("{entry}/");
            list.retain(|e| e != entry && !e.starts_with(&prefix));
        }
    }
    let mut seen = std::collections::HashSet::new();
    list.retain(|e| seen.insert(e.clone()));
    list
}

/// Returns `config` with `entry` added to or removed from an explicit `engines` list,
/// which [`edited_entries`] edits; without a list, nothing changes.
fn edit_engines(config: &str, entry: &str, enable: bool) -> anyhow::Result<String> {
    let doc: toml_edit::DocumentMut = config.parse()?;
    let Some(item) = doc.get("engines") else {
        return Ok(config.to_owned());
    };
    let list: Vec<String> = item
        .as_array()
        .context("`engines` must be a list of engine ids")?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let list = edited_entries(list, entry, enable, &[]);
    omime_proto::config::edit(config, "engines", omime_proto::config::Value::List(list))
        .map_err(anyhow::Error::msg)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(Some(command)) => command,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("omime: {e}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("omime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(command)) {
        Ok(code) => code,
        Err(e) => {
            let message = match e.downcast_ref::<zbus::Error>() {
                Some(e @ zbus::Error::MethodError(..)) => dbus_message(e),
                _ => format!("{e:#}"),
            };
            eprintln!("omime: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(parse(&args("")), Ok(None)));
        assert!(matches!(
            parse(&args("list")),
            Ok(Some(Command::Daemon(DaemonCommand::List)))
        ));
        assert!(matches!(
            parse(&args("engines")),
            Ok(Some(Command::Daemon(DaemonCommand::Engines)))
        ));
        assert!(matches!(
            parse(&args("status --json")),
            Ok(Some(Command::Daemon(DaemonCommand::Status { json: true })))
        ));
        assert!(matches!(
            parse(&args("status --watch")),
            Ok(Some(Command::Watch { json: false }))
        ));
        assert!(matches!(
            parse(&args("status --json --watch")),
            Ok(Some(Command::Watch { json: true }))
        ));
        assert!(matches!(
            parse(&args("status --watch --json")),
            Ok(Some(Command::Watch { json: true }))
        ));
        assert!(
            matches!(parse(&args("switch echo")), Ok(Some(Command::Daemon(DaemonCommand::Switch(e)))) if e == "echo")
        );
        assert!(matches!(
            parse(&args("settings")),
            Ok(Some(Command::OwnSettings))
        ));
        assert!(
            matches!(parse(&args("settings fcitx5")), Ok(Some(Command::Daemon(DaemonCommand::Settings(e)))) if e == "fcitx5")
        );
        assert!(matches!(
            parse(&args("install /tmp/echo")),
            Ok(Some(Command::Install { dir, force: false })) if dir == Path::new("/tmp/echo")
        ));
        assert!(matches!(
            parse(&args("install /tmp/echo --force")),
            Ok(Some(Command::Install { force: true, .. }))
        ));
        assert!(
            matches!(parse(&args("uninstall echo")), Ok(Some(Command::Uninstall(e))) if e == "echo")
        );
        assert!(matches!(parse(&args("doctor")), Ok(Some(Command::Doctor))));
        assert!(parse(&args("doctor --json")).is_err());
        assert!(parse(&args("install")).is_err());
        assert!(parse(&args("install --force")).is_err());
        assert!(matches!(
            parse(&args("install --force /tmp/echo")),
            Ok(Some(Command::Install { force: true, .. }))
        ));
        assert!(parse(&args("uninstall")).is_err());
        assert!(parse(&args("switch")).is_err());
        assert!(parse(&args("frobnicate")).is_err());
        assert!(matches!(
            parse(&args("list --all")),
            Ok(Some(Command::Daemon(DaemonCommand::ListAll)))
        ));
        assert!(matches!(
            parse(&args("config")),
            Ok(Some(Command::Daemon(DaemonCommand::Config(None))))
        ));
        assert!(matches!(
            parse(&args("config apps.remember true")),
            Ok(Some(Command::Daemon(DaemonCommand::Config(Some(_)))))
        ));
        assert!(parse(&args("config apps.remember")).is_err());
        assert!(matches!(
            parse(&args("properties")),
            Ok(Some(Command::Daemon(DaemonCommand::Properties)))
        ));
        assert!(
            matches!(parse(&args("activate punct")), Ok(Some(Command::Daemon(DaemonCommand::Activate(id)))) if id == "punct")
        );
        assert!(parse(&args("activate")).is_err());
    }

    #[test]
    fn menus_are_indented_with_the_state_of_switches() {
        use omime_proto::engine::{Property, PropertyKind};
        let property = |id: &str, parent: &str, kind, checked| Property {
            id: id.into(),
            parent: parent.into(),
            kind,
            label: id.to_uppercase(),
            checked,
            ..Property::default()
        };
        let menu: Vec<_> = [
            property("punct", "", PropertyKind::Toggle, true),
            property("width", "", PropertyKind::Menu, false),
            property("full", "width", PropertyKind::Radio, false),
            property("half", "width", PropertyKind::Radio, true),
            property("sep", "", PropertyKind::Separator, false),
            property("deploy", "", PropertyKind::Action, false),
        ]
        .iter()
        .map(Property::to_dict)
        .collect();
        let lines: String = menu.iter().map(|p| property_line(p, &menu)).collect();
        assert_eq!(
            lines,
            "[x] PUNCT  (punct)\nWIDTH  (width)\n  [ ] FULL  (full)\n  [x] HALF  (half)\n-\n\
             DEPLOY  (deploy)\n"
        );
    }

    #[test]
    fn editing_engines_keeps_the_rest_of_the_file() {
        let config = "# my settings\nengines = [\"echo\"]\n\n[hotkeys]\nnext = \"Super+space\"\n";
        let enabled = edit_engines(config, "sparkle", true).unwrap();
        assert!(enabled.starts_with("# my settings\nengines = [\"echo\", \"sparkle\"]"));
        assert!(enabled.contains("next = \"Super+space\""));
        let disabled = edit_engines(&enabled, "echo", false).unwrap();
        assert!(disabled.contains("engines = [\"sparkle\"]"));
        // Without a list, every installed engine is enabled already.
        assert_eq!(edit_engines("", "echo", true).unwrap(), "");
    }

    #[test]
    fn input_methods_inside_frameworks_are_entries_of_their_own() {
        let inside = ["fcitx5/shuangpin".to_owned(), "fcitx5/pinyin".to_owned()];
        let list = |entries: &[&str]| entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>();
        // One more input method of Fcitx 5.
        let entries = edited_entries(list(&["fcitx5", "sparkle"]), "fcitx5/rime", true, &inside);
        assert_eq!(entries, list(&["fcitx5", "sparkle", "fcitx5/rime"]));
        // Fcitx 5 without pinyin: its other input methods stay, in place.
        let entries = edited_entries(entries, "fcitx5/pinyin", false, &inside);
        assert_eq!(
            entries,
            list(&["fcitx5/shuangpin", "sparkle", "fcitx5/rime"])
        );
        // No Fcitx 5 at all.
        let entries = edited_entries(entries, "fcitx5", false, &inside);
        assert_eq!(entries, list(&["sparkle"]));
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
        assert_eq!(json_string("中"), "\"中\"");
    }

    #[test]
    fn status_lines_match_waybar() {
        let status = HashMap::from([(
            "engine".to_owned(),
            OwnedVariant::try_from(zbus::Variant::from("echo")).unwrap(),
        )]);
        assert_eq!(format_status(&status, false), "echo");
        assert_eq!(
            format_status(&status, true),
            "{\"engine\":\"echo\",\"input_method\":\"echo\",\"label\":null,\"tooltip\":null,\
             \"text\":\"echo\",\"class\":\"echo\"}"
        );
        assert_eq!(
            format_status(&HashMap::new(), true),
            "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":null,\
             \"text\":\"⌨\",\"class\":\"direct\"}"
        );
        assert_eq!(format_stopped(false), "omimed is not running");
        assert_eq!(
            format_stopped(true),
            "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":\"omimed is not running\",\
             \"text\":\"⌨\",\"class\":\"stopped\"}"
        );
    }
}
