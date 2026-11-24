//! Desktop-owned input lifecycles: KWin launches an input method on its privileged
//! socket, while GNOME keeps IBus and loads oh-my-ime as one of its engines.

use super::*;

const KDE_ENTRY: &str = "io.github.ign1x.OhMyIme.InputMethod.desktop";
const IBUS_AUTOSTART: &str = "io.github.ign1x.OhMyIme.IBus.desktop";

fn desktop_command(path: &Path, args: &str) -> anyhow::Result<String> {
    // Desktop Exec has two escape layers; percent is a field code even in quotes.
    let path = path_text(path)?
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\\\"")
        .replace('`', "\\\\`")
        .replace('$', "\\\\$")
        .replace('%', "%%");
    Ok(format!("\"{path}\"{args}"))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn prepare(
    locations: &Locations,
    options: &Options,
    daemon: PathBuf,
    installed: &[manifest::InstalledEngine],
    previous: Option<Record>,
    desktop: Desktop,
) -> anyhow::Result<Plan> {
    ensure!(
        options.config.is_none(),
        "{}",
        tr!(
            "Native KDE/GNOME setup uses desktop registration files; --config applies only to Hyprland and Sway."
        )
    );
    let engine = options
        .engine
        .clone()
        .or_else(|| previous.as_ref().and_then(|p| p.engine.clone()));
    if let Some(target) = engine.as_deref().filter(|e| *e != "direct") {
        let id = target.split('/').next().unwrap_or(target);
        let found = installed.iter().find(|e| e.id() == id)
            .with_context(|| tr!("input method engine `{id}` is not installed or its required program is unavailable (see omime doctor)", id = id))?;
        if let Some(problem) = crate::install::command_warning(found) {
            bail!("{problem}");
        }
        ensure!(
            desktop != Desktop::Gnome || id == "rime" || id == "echo",
            "{}",
            tr!("GNOME native presentation currently requires Rime or Echo; choose --engine rime.")
        );
    }
    let root = locations
        .config_dir
        .parent()
        .context(tr!("no configuration directory"))?;
    let registration = match desktop {
        Desktop::Kde => locations.data_dir.join("applications").join(KDE_ENTRY),
        Desktop::Gnome => locations.data_dir.join("ibus/component/oh-my-ime.xml"),
        _ => unreachable!("native desktops only"),
    };
    let mut record = Record {
        version: 1,
        desktop,
        complete: true,
        config_path: registration.clone(),
        engine: engine.clone(),
        take_over: false,
        files: Vec::new(),
    };
    let mut changes = Vec::new();
    let base = |path: &Path| -> anyhow::Result<(Option<String>, Option<String>)> {
        let current = files::read(path)?;
        let original = match previous
            .as_ref()
            .and_then(|r| r.files.iter().find(|f| f.path == path))
        {
            Some(file) => files::unmanage(file, current.as_deref())?,
            None => current.clone(),
        };
        Ok((current, original))
    };
    let mut whole = |path: PathBuf, text: String| -> anyhow::Result<()> {
        let (current, original) = base(&path)?;
        ensure!(
            original.is_none() || original.as_deref() == Some(&text),
            "{}",
            tr!("{arg0} already exists outside setup", arg0 = path.display())
        );
        add_file(
            &mut record,
            &mut changes,
            path,
            current,
            original,
            text,
            Kind::Whole,
        );
        Ok(())
    };
    match desktop {
        Desktop::Kde => whole(
            registration,
            include_str!("../../../../data/io.github.ign1x.OhMyIme.InputMethod.desktop").replace(
                "Exec=omimed",
                &format!("Exec={}", desktop_command(&daemon, "")?),
            ),
        )?,
        Desktop::Gnome => {
            // IBus parses exec using GLib shell syntax (not desktop Exec field codes).
            let command = format!("{} --ibus", shell_quote(path_text(&daemon)?));
            whole(
                registration,
                include_str!("../../../../data/ibus/oh-my-ime.xml").replace(
                    "<exec>omimed --ibus</exec>",
                    &format!("<exec>{}</exec>", xml(&command)),
                ),
            )?;
            whole(
                root.join("autostart").join(IBUS_AUTOSTART),
                include_str!("../../../../data/io.github.ign1x.OhMyIme.IBus.desktop").replace(
                    "Exec=omimed --ibus",
                    &format!("Exec={}", desktop_command(&daemon, " --ibus")?),
                ),
            )?;
        }
        _ => unreachable!("native desktops only"),
    }
    let config_path = locations.config_dir.join(config::CONFIG_FILE);
    let (current, original) = base(&config_path)?;
    let text = original.as_deref().unwrap_or("");
    let (config, _) = Config::parse(text).map_err(anyhow::Error::msg)?;
    if let Some(target) = engine {
        let mut edited = config::edit(
            text,
            "initial_input_method",
            config::Value::String(target.clone()),
        )
        .map_err(anyhow::Error::msg)?;
        let mut keys = vec!["initial_input_method".to_owned()];
        let id = target.split('/').next().unwrap_or(&target);
        if id != "direct" {
            let mut enabled = config.engines.unwrap_or_else(|| {
                installed
                    .iter()
                    .filter(|e| e.manifest.engine.enabled_by_default)
                    .map(|e| e.id().to_owned())
                    .collect()
            });
            if !enabled.iter().any(|e| e == id || e == &target) {
                enabled.push(target);
                edited = config::edit(&edited, "engines", config::Value::List(enabled))
                    .map_err(anyhow::Error::msg)?;
                keys.push("engines".into());
            }
        }
        add_file(
            &mut record,
            &mut changes,
            config_path,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_paths_escape_desktop_and_xml_grammars() {
        assert_eq!(
            desktop_command(Path::new("/a b/100%/omimed"), " --ibus").unwrap(),
            "\"/a b/100%%/omimed\" --ibus"
        );
        assert!(desktop_command(Path::new("/bad\npath"), "").is_err());
        assert_eq!(xml("'a&b<\"'"), "&apos;a&amp;b&lt;&quot;&apos;");
    }
}
