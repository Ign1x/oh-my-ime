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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ApiVersion;

/// File name of a manifest inside an engine directory.
pub const MANIFEST_FILE: &str = "engine.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub engine: EngineInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Engine protocol version, `"MAJOR.MINOR"`.
    pub api: String,
    /// Translations, keyed by locale (`zh-CN`, `ja`, ...).
    #[serde(default)]
    pub i18n: BTreeMap<String, Translation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Translation {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
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
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self =
            toml::from_str(text).map_err(|e| ManifestError::Syntax(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
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
        if parse_api_version(&e.api).is_none() {
            return Err(ManifestError::Invalid(format!(
                "`api = \"{}\"` is not MAJOR.MINOR",
                e.api
            )));
        }
        Ok(())
    }

    pub fn api_version(&self) -> ApiVersion {
        parse_api_version(&self.engine.api).unwrap_or(ApiVersion { major: 0, minor: 0 })
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
}

impl InstalledEngine {
    pub fn id(&self) -> &str {
        &self.manifest.engine.id
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

/// Loads every engine found in `dirs`, in order. An id found in an earlier directory
/// shadows the same id in later ones, so user installs override system ones.
///
/// Broken manifests are reported in the second list instead of aborting.
pub fn discover(dirs: &[PathBuf]) -> (Vec<InstalledEngine>, Vec<(PathBuf, String)>) {
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
                Ok(manifest) => {
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
                        });
                }
                Err(e) => problems.push((path, e.to_string())),
            }
        }
    }
    (engines.into_values().collect(), problems)
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
        let manifest = Manifest::parse(ECHO).unwrap();
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
        let unknown = format!("{ECHO}\n[extra]\n");
        assert!(matches!(
            Manifest::parse(&unknown),
            Err(ManifestError::Syntax(_))
        ));
    }

    #[test]
    fn resolves_relative_programs() {
        let engine = InstalledEngine {
            manifest: Manifest::parse(ECHO).unwrap(),
            dir: PathBuf::from("/usr/share/oh-my-ime/engines/echo"),
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

        let (engines, problems) = discover(&[user.clone(), system.clone()]);
        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0].manifest.engine.name, "User Echo");
        assert_eq!(problems.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
