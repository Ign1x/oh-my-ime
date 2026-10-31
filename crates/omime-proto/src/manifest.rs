//! Engine manifests (`engine.toml`).
//!
//! An engine is installed by placing a directory named after its id, containing an
//! `engine.toml`, in one of the engine directories (see [`crate::paths`]):
//!
//! ```toml
//! [engine]
//! id = "echo"
//! name = "Echo"
//! description = "Turns words into UPPERCASE; a test and tutorial engine"
//! languages = ["en"]
//! exec = ["omime-echo"]
//! api = "1.0"
//!
//! [engine.i18n.zh-CN]
//! name = "回声"
//! ```
//!
//! A relative `exec[0]` that contains a `/` is resolved against the manifest's directory;
//! a bare name is looked up in `$PATH`.
//!
//! An unmodified Wayland input method, such as Fcitx5, runs as an engine too. It speaks
//! `zwp_input_method_v2` instead of the engine protocol, so its manifest has no `api`.
//! A manifest shipped for a program that may not be installed names it in `requires`,
//! and is offered rather than enabled:
//!
//! ```toml
//! [engine]
//! id = "fcitx5"
//! name = "Fcitx 5"
//! exec = ["fcitx5"]
//! protocol = "input-method-v2"
//! requires = ["fcitx5"]
//! enabled_by_default = false
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ApiVersion;

/// File name of a manifest inside an engine directory.
pub const MANIFEST_FILE: &str = "engine.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub engine: EngineInfo,
}

/// How the daemon talks to an engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// The engine protocol, `io.github.ign1x.OhMyIme.Engine1` over D-Bus.
    #[default]
    Omime,
    /// A Wayland input method (`zwp_input_method_v2` and `zwp_virtual_keyboard_v1`),
    /// hosted on a virtual input method of the daemon's.
    InputMethodV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// BCP 47 language tags the engine is meant for.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Icon name from the icon theme.
    #[serde(default)]
    pub icon: Option<String>,
    /// Command line that starts the engine.
    pub exec: Vec<String>,
    #[serde(default)]
    pub protocol: Protocol,
    /// Engine protocol version, `"MAJOR.MINOR"`; required with [`Protocol::Omime`].
    #[serde(default)]
    pub api: Option<String>,
    /// Programs that must be installed for the engine to be offered at all: names
    /// looked up in `$PATH`, or absolute paths.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Whether the engine is enabled while `config.toml` lists no engines. Engines for
    /// other frameworks are offered, not imposed, so their manifests say `false`.
    #[serde(default = "enabled_by_default")]
    pub enabled_by_default: bool,
    /// Translations, keyed by locale (`zh-CN`, `ja`, ...).
    #[serde(default)]
    pub i18n: BTreeMap<String, Translation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Translation {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

/// Errors in a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Syntax(String),
    Invalid(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(e) => write!(f, "syntax error: {e}"),
            Self::Invalid(e) => write!(f, "invalid manifest: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Returns whether `id` is a valid engine id: 1-64 characters from `[A-Za-z0-9._-]`,
/// not starting with a dot. Ids become path components and D-Bus-safe labels.
pub fn is_valid_engine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Parses `"MAJOR.MINOR"`.
pub fn parse_api_version(text: &str) -> Option<ApiVersion> {
    let (major, minor) = text.split_once('.')?;
    Some(ApiVersion {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
    })
}

impl Manifest {
    /// Reads and checks a manifest. Keys this version does not know, perhaps from a
    /// manifest written for a newer one, are left out and returned.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), ManifestError> {
        let (manifest, unknown): (Self, _) =
            crate::from_toml(text).map_err(ManifestError::Syntax)?;
        manifest.validate()?;
        Ok((manifest, unknown))
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let e = &self.engine;
        if !is_valid_engine_id(&e.id) {
            return Err(ManifestError::Invalid(format!(
                "`{}` is not a valid id (1-64 characters from [A-Za-z0-9._-])",
                e.id
            )));
        }
        if e.name.trim().is_empty() {
            return Err(ManifestError::Invalid("`name` must not be empty".into()));
        }
        if e.exec.first().is_none_or(|program| program.is_empty()) {
            return Err(ManifestError::Invalid("`exec` needs a program".into()));
        }
        match (&e.api, e.protocol) {
            (Some(api), _) if parse_api_version(api).is_none() => {
                return Err(ManifestError::Invalid(format!(
                    "`api = \"{api}\"` is not MAJOR.MINOR"
                )));
            }
            (None, Protocol::Omime) => {
                return Err(ManifestError::Invalid(
                    "`api` is missing: which engine protocol version does the engine speak?".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// The engine protocol version the engine speaks; 0.0 for hosted input methods.
    pub fn api_version(&self) -> ApiVersion {
        self.engine
            .api
            .as_deref()
            .and_then(parse_api_version)
            .unwrap_or(ApiVersion { major: 0, minor: 0 })
    }

    /// Whether the engine is a hosted Wayland input method.
    pub fn is_hosted(&self) -> bool {
        self.engine.protocol == Protocol::InputMethodV2
    }

    /// The engine's name in `locale` (e.g. `zh-CN` or `zh_CN.UTF-8`), falling back to
    /// the language alone and then to the untranslated name.
    pub fn localized_name(&self, locale: &str) -> &str {
        let locale = locale.split('.').next().unwrap_or(locale).replace('_', "-");
        let language = locale.split('-').next().unwrap_or(&locale);
        [locale.as_str(), language]
            .iter()
            .find_map(|l| self.engine.i18n.get(*l).and_then(|t| t.name.as_deref()))
            .unwrap_or(&self.engine.name)
    }
}

/// A manifest together with the directory it was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledEngine {
    pub manifest: Manifest,
    pub dir: PathBuf,
    /// Keys of the manifest this version does not know and left out.
    pub unknown_keys: Vec<String>,
}

impl InstalledEngine {
    pub fn id(&self) -> &str {
        &self.manifest.engine.id
    }

    /// The first program of `requires` that is not installed, if any.
    pub fn missing_requirement(&self) -> Option<&str> {
        self.manifest
            .engine
            .requires
            .iter()
            .find(|program| !is_installed(program))
            .map(String::as_str)
    }

    /// The command line with `exec[0]` resolved as documented in the module docs.
    pub fn command(&self) -> Vec<String> {
        let mut argv = self.manifest.engine.exec.clone();
        if let Some(program) = argv.first_mut()
            && program.contains('/')
            && !Path::new(program).is_absolute()
        {
            *program = self.dir.join(&*program).to_string_lossy().into_owned();
        }
        argv
    }
}

/// Whether `program`, a name looked up in `$PATH` or a path, is an executable file.
fn is_installed(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let executable = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(Path::new(program));
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(program))))
}

/// What [`discover`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub engines: Vec<InstalledEngine>,
    /// Manifests that could not be loaded, and why.
    pub broken: Vec<(PathBuf, String)>,
    /// Engines left out because a program in their `requires` is not installed:
    /// (id, program).
    pub unavailable: Vec<(String, String)>,
}

/// Loads every engine found in `dirs`, in order. An id found in an earlier directory
/// shadows the same id in later ones, so user installs override system ones.
///
/// Broken manifests are reported instead of aborting, and engines whose `requires` are
/// not installed are left out.
pub fn discover(dirs: &[PathBuf]) -> Discovery {
    let mut engines: BTreeMap<String, InstalledEngine> = BTreeMap::new();
    let mut problems = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for engine_dir in entries {
            let path = engine_dir.join(MANIFEST_FILE);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Manifest::parse(&text) {
                Ok((manifest, unknown_keys)) => {
                    let dir_name = engine_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    if dir_name.as_deref() != Some(manifest.engine.id.as_str()) {
                        problems.push((
                            path,
                            format!(
                                "the directory must be named after the id `{}`",
                                manifest.engine.id
                            ),
                        ));
                        continue;
                    }
                    engines
                        .entry(manifest.engine.id.clone())
                        .or_insert(InstalledEngine {
                            manifest,
                            dir: engine_dir,
                            unknown_keys,
                        });
                }
                Err(e) => problems.push((path, e.to_string())),
            }
        }
    }
    let mut found = Discovery {
        broken: problems,
        ..Discovery::default()
    };
    for engine in engines.into_values() {
        match engine.missing_requirement() {
            Some(program) => found
                .unavailable
                .push((engine.id().to_owned(), program.to_owned())),
            None => found.engines.push(engine),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECHO: &str = r#"
[engine]
id = "echo"
name = "Echo"
languages = ["en"]
exec = ["bin/omime-echo", "--flag"]
api = "1.0"

[engine.i18n.zh-CN]
name = "回声"
"#;

    #[test]
    fn parses_and_localizes() {
        let (manifest, unknown) = Manifest::parse(ECHO).unwrap();
        assert!(unknown.is_empty());
        assert_eq!(manifest.engine.id, "echo");
        assert_eq!(manifest.api_version(), ApiVersion { major: 1, minor: 0 });
        assert_eq!(manifest.localized_name("zh_CN.UTF-8"), "回声");
        assert_eq!(manifest.localized_name("zh-CN"), "回声");
        assert_eq!(manifest.localized_name("de_DE"), "Echo");
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad_id = ECHO.replace("id = \"echo\"", "id = \"../x\"");
        assert!(matches!(
            Manifest::parse(&bad_id),
            Err(ManifestError::Invalid(_))
        ));
        let no_exec = ECHO.replace("exec = [\"bin/omime-echo\", \"--flag\"]", "exec = []");
        assert!(Manifest::parse(&no_exec).is_err());
        let bad_api = ECHO.replace("api = \"1.0\"", "api = \"one\"");
        assert!(Manifest::parse(&bad_api).is_err());
        let no_api = ECHO.replace("api = \"1.0\"", "");
        assert!(Manifest::parse(&no_api).is_err());
    }

    #[test]
    fn unknown_keys_are_left_out() {
        let newer = ECHO.replace(
            "api = \"1.0\"",
            "api = \"1.0\"\nsettings = [\"echo-setup\"]",
        );
        let (manifest, unknown) = Manifest::parse(&format!("{newer}\n[extra]\n")).unwrap();
        assert_eq!(manifest.engine.id, "echo");
        assert_eq!(unknown, ["engine.settings", "extra"]);
    }

    #[test]
    fn hosted_input_methods_need_no_api() {
        let fcitx5 = r#"
[engine]
id = "fcitx5"
name = "Fcitx 5"
exec = ["fcitx5"]
protocol = "input-method-v2"
"#;
        let (manifest, _) = Manifest::parse(fcitx5).unwrap();
        assert!(manifest.is_hosted());
        assert_eq!(manifest.api_version(), ApiVersion { major: 0, minor: 0 });
        assert!(!Manifest::parse(ECHO).unwrap().0.is_hosted());
        let unknown = fcitx5.replace("input-method-v2", "input-method-v1");
        assert!(matches!(
            Manifest::parse(&unknown),
            Err(ManifestError::Syntax(_))
        ));
    }

    #[test]
    fn resolves_relative_programs() {
        let engine = InstalledEngine {
            manifest: Manifest::parse(ECHO).unwrap().0,
            dir: PathBuf::from("/usr/share/oh-my-ime/engines/echo"),
            unknown_keys: Vec::new(),
        };
        assert_eq!(
            engine.command(),
            ["/usr/share/oh-my-ime/engines/echo/bin/omime-echo", "--flag"]
        );
        let mut on_path = engine.clone();
        on_path.manifest.engine.exec = vec!["omime-echo".into()];
        assert_eq!(on_path.command(), ["omime-echo"]);
    }

    #[test]
    fn discovery_prefers_earlier_directories() {
        let root = std::env::temp_dir().join(format!("omime-discover-{}", std::process::id()));
        let user = root.join("user");
        let system = root.join("system");
        for (dir, name) in [(&user, "User Echo"), (&system, "System Echo")] {
            std::fs::create_dir_all(dir.join("echo")).unwrap();
            std::fs::write(
                dir.join("echo").join(MANIFEST_FILE),
                ECHO.replace("name = \"Echo\"", &format!("name = \"{name}\"")),
            )
            .unwrap();
        }
        std::fs::create_dir_all(system.join("wrong-dir")).unwrap();
        std::fs::write(system.join("wrong-dir").join(MANIFEST_FILE), ECHO).unwrap();

        let found = discover(&[user.clone(), system.clone()]);
        assert_eq!(found.engines.len(), 1);
        assert_eq!(found.engines[0].manifest.engine.name, "User Echo");
        assert_eq!(found.broken.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engines_need_what_they_require() {
        let root = std::env::temp_dir().join(format!("omime-requires-{}", std::process::id()));
        // `sh` is everywhere; the other program is nowhere.
        for (id, requires) in [("there", "sh"), ("missing", "omime-no-such-program")] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(
                root.join(id).join(MANIFEST_FILE),
                ECHO.replace("id = \"echo\"", &format!("id = \"{id}\""))
                    .replace(
                        "api = \"1.0\"",
                        &format!("api = \"1.0\"\nrequires = [\"{requires}\"]"),
                    ),
            )
            .unwrap();
        }
        let found = discover(std::slice::from_ref(&root));
        let ids: Vec<&str> = found.engines.iter().map(InstalledEngine::id).collect();
        assert_eq!(ids, ["there"]);
        assert_eq!(
            found.unavailable,
            [("missing".to_owned(), "omime-no-such-program".to_owned())]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engines_are_enabled_by_default_unless_they_say_otherwise() {
        assert!(Manifest::parse(ECHO).unwrap().0.engine.enabled_by_default);
        let offered = ECHO.replace("api = \"1.0\"", "api = \"1.0\"\nenabled_by_default = false");
        assert!(
            !Manifest::parse(&offered)
                .unwrap()
                .0
                .engine
                .enabled_by_default
        );
    }
}
