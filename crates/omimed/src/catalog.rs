//! What omimed knows about the input methods of hosted frameworks, kept across restarts
//! in `$XDG_CACHE_HOME/oh-my-ime/catalog.toml`, so that every input method of a
//! framework can be listed before the framework runs again. A cache: losing it costs a
//! framework start, nothing more. Unlike the state, it changes only when a framework
//! reports something new.
//!
//! A framework that never ran has told omimed nothing; then the files it installs say
//! what it has ([`installed`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::core::frameworks::{InnerMethod, is_layout};

const CATALOG_FILE: &str = "catalog.toml";
/// How long a program may take to list an IBus component's engines.
const LISTING_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    /// Every input method of each framework, set up or not, by engine.
    pub available: BTreeMap<String, Vec<InnerMethod>>,
}

fn path() -> Option<PathBuf> {
    omime_proto::paths::cache_dir().map(|d| d.join(CATALOG_FILE))
}

impl Catalog {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                warn!("ignoring {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the catalog atomically (write to a temporary file, then rename).
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let result = (|| -> std::io::Result<()> {
            let text = toml::to_string(self).map_err(std::io::Error::other)?;
            omime_proto::paths::atomic_write(&path, text.as_bytes(), None, false)
        })();
        if let Err(e) = result {
            warn!("cannot save {}: {e}", path.display());
        }
    }
}

/// The input methods of a framework installed in `prefixes` (`/usr`, a Nix store path),
/// as the files it installs describe them: Fcitx 5's `share/fcitx5/inputmethod/*.conf`
/// and the engines of IBus's components, `share/ibus/component/*.xml`, without keyboard
/// layouts. Components that list their engines with a program (`<engines exec="...">`)
/// are asked, as IBus asks them.
pub async fn installed(prefixes: &[PathBuf]) -> Vec<InnerMethod> {
    let locales = locales();
    let mut methods = Vec::new();
    for prefix in prefixes {
        methods.extend(fcitx_installed(
            &prefix.join("share/fcitx5/inputmethod"),
            &locales,
        ));
        methods.extend(ibus_installed(&prefix.join("share/ibus/component")).await);
    }
    let mut seen = HashSet::new();
    methods.retain(|m| !is_layout(&m.id) && seen.insert(m.id.clone()));
    methods
}

/// The files in `dir` with the extension `extension`, by name.
fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .collect();
    files.sort();
    files
}

/// The user's locale and its language, as Fcitx 5 names translations: `zh_CN`, `zh`.
fn locales() -> Vec<String> {
    locale_names(&omime_proto::i18n::preferences())
}

fn locale_names(preferences: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for locale in preferences {
        for tag in omime_proto::i18n::candidates(locale) {
            // Fcitx's Name is the English fallback; do not skip an explicit
            // English preference to find a later language in LANGUAGE.
            if matches!(tag.as_str(), "en" | "c" | "posix") {
                return result;
            }
            let name = tag
                .split('-')
                .enumerate()
                .map(|(index, part)| {
                    if index == 0 {
                        part.to_owned()
                    } else if part.len() == 4 && part.is_ascii() {
                        format!("{}{}", part[..1].to_ascii_uppercase(), &part[1..])
                    } else {
                        part.to_ascii_uppercase()
                    }
                })
                .collect::<Vec<_>>()
                .join("_");
            if !result.contains(&name) {
                result.push(name);
            }
        }
    }
    result
}

fn fcitx_installed(dir: &Path, locales: &[String]) -> Vec<InnerMethod> {
    files(dir, "conf")
        .iter()
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?;
            fcitx_input_method(id, &std::fs::read_to_string(path).ok()?, locales)
        })
        .collect()
}

/// An input method of Fcitx 5, from its `inputmethod/<id>.conf`, named in the first of
/// `locales` it has a name in.
fn fcitx_input_method(id: &str, text: &str, locales: &[String]) -> Option<InnerMethod> {
    let mut section = "";
    let mut values = HashMap::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name;
        } else if section == "InputMethod"
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            values.insert(key.trim(), value);
        }
    }
    let value = |key: &str| values.get(key).map(|v| (*v).to_owned());
    let name = locales
        .iter()
        .find_map(|locale| value(&format!("Name[{locale}]")))
        .or_else(|| value("Name"))?;
    Some(InnerMethod {
        id: id.to_owned(),
        name,
        icon: value("Icon").unwrap_or_default(),
        label: value("Label").unwrap_or_default(),
        language: value("LangCode").unwrap_or_default(),
    })
}

async fn ibus_installed(dir: &Path) -> Vec<InnerMethod> {
    let mut engines = Vec::new();
    for path in files(dir, "xml") {
        let Ok(xml) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (listed, exec) = ibus_engines(&xml);
        engines.extend(listed);
        if let Some(exec) = exec
            && let Some(xml) = run_listing(&exec).await
        {
            engines.extend(ibus_engines(&xml).0);
        }
    }
    engines
}

/// The engines in an IBus component, or in what a component's program lists, and the
/// program that lists more (`<engines exec="...">`).
fn ibus_engines(xml: &str) -> (Vec<InnerMethod>, Option<String>) {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut engines = Vec::new();
    let mut exec = None;
    // The elements the reader is in, and the fields of the engine it is in.
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut engine: Option<HashMap<Vec<u8>, String>> = None;
    let mut text = String::new();
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(e) => {
                debug!("a broken IBus component: {e}");
                break;
            }
        };
        let start = matches!(event, Event::Start(_));
        match event {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == b"engines" =>
            {
                if let Ok(Some(attribute)) = element.try_get_attribute("exec")
                    && let Ok(value) =
                        attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                {
                    exec = exec.or(Some(value.into_owned()));
                }
                if start {
                    path.push(b"engines".to_vec());
                }
            }
            Event::Start(element) => {
                let name = element.local_name().as_ref().to_vec();
                if name == b"engine" {
                    engine = Some(HashMap::new());
                }
                path.push(name);
                text.clear();
            }
            Event::Text(t) => text.push_str(&t.xml10_content().unwrap_or_default()),
            Event::CData(t) => text.push_str(&t.decode().unwrap_or_default()),
            Event::GeneralRef(reference) => {
                if let Ok(Some(c)) = reference.resolve_char_ref() {
                    text.push(c);
                } else if let Ok(name) = reference.decode()
                    && let Some(resolved) = quick_xml::escape::resolve_predefined_entity(&name)
                {
                    text.push_str(resolved);
                }
            }
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                if name == b"engine" {
                    if let Some(fields) = engine.take()
                        && let Some(method) = ibus_engine(&fields)
                    {
                        engines.push(method);
                    }
                } else if path.last().is_some_and(|parent| parent == b"engine")
                    && let Some(fields) = &mut engine
                {
                    fields.insert(name, text.trim().to_owned());
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (engines, exec)
}

/// An engine of IBus from the fields of its `<engine>`; `None` without a name.
fn ibus_engine(fields: &HashMap<Vec<u8>, String>) -> Option<InnerMethod> {
    let field = |name: &[u8]| fields.get(name).filter(|v| !v.is_empty()).cloned();
    let id = field(b"name")?;
    Some(InnerMethod {
        name: field(b"longname").unwrap_or_else(|| id.clone()),
        icon: field(b"icon").unwrap_or_default(),
        label: field(b"symbol").unwrap_or_default(),
        language: field(b"language").unwrap_or_default(),
        id,
    })
}

/// What `command` prints, if it succeeds within [`LISTING_TIMEOUT`]. Its words are its
/// arguments: IBus's components name programs without quoting.
async fn run_listing(command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    let program = words.next()?;
    let child = tokio::process::Command::new(program)
        .args(words)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .inspect_err(|e| debug!(command, "cannot list IBus engines: {e}"))
        .ok()?;
    match tokio::time::timeout(LISTING_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout).ok(),
        Ok(Ok(output)) => {
            debug!(command, status = %output.status, "cannot list IBus engines");
            None
        }
        Ok(Err(e)) => {
            debug!(command, "cannot list IBus engines: {e}");
            None
        }
        Err(_) => {
            debug!(command, "listing IBus engines takes too long");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_preferences_keep_scripts_and_stop_at_english() {
        assert_eq!(
            locale_names(&["en".into(), "zh_CN".into()]),
            Vec::<String>::new()
        );
        assert_eq!(
            locale_names(&["zh-Hans-CN".into()]),
            ["zh_Hans_CN", "zh_CN", "zh_Hans", "zh"]
        );
        let locales = locale_names(&["fr".into(), "zh_CN".into(), "en".into()]);
        assert_eq!(locales, ["fr", "zh_CN", "zh"]);
    }

    #[test]
    fn fcitx5_input_methods_are_read_in_the_users_language() {
        let conf = "[InputMethod]\n# A comment\nName=Pinyin\nName[zh_CN]=拼音\nIcon=fcitx-pinyin\n\
                    Label=拼\nLangCode=zh_CN\nAddon=pinyin\n\n[Other]\nName=Not this\n";
        let chinese = ["zh_CN".to_owned(), "zh".to_owned()];
        assert_eq!(
            fcitx_input_method("pinyin", conf, &chinese),
            Some(InnerMethod {
                id: "pinyin".to_owned(),
                name: "拼音".to_owned(),
                icon: "fcitx-pinyin".to_owned(),
                label: "拼".to_owned(),
                language: "zh_CN".to_owned(),
            })
        );
        let english = fcitx_input_method("pinyin", conf, &["en_US".to_owned()]);
        assert_eq!(english.map(|m| m.name).as_deref(), Some("Pinyin"));
        // Only the language matches.
        let taiwan = "[InputMethod]\nName=Zhuyin\nName[zh]=注音\n";
        let zhuyin = fcitx_input_method("zhuyin", taiwan, &chinese);
        assert_eq!(zhuyin.map(|m| m.name).as_deref(), Some("注音"));
        assert_eq!(
            fcitx_input_method("x", "[Addon]\nName=Pinyin\n", &chinese),
            None
        );
    }

    #[test]
    fn ibus_components_list_engines() {
        let component = r#"<?xml version="1.0" encoding="utf-8"?>
<component>
  <name>org.freedesktop.IBus.Test</name>
  <author>Someone &lt;someone@example.org&gt;</author>
  <engines>
    <engine>
      <name>hangul</name>
      <language>ko</language>
      <author>A &amp; B</author>
      <longname>Hangul</longname>
      <symbol>&#xD55C;</symbol>
      <icon>ibus-hangul</icon>
    </engine>
    <engine>
      <longname>No name</longname>
    </engine>
    <engine>
      <name>xkb:us::eng</name>
      <longname><![CDATA[English & more]]></longname>
    </engine>
  </engines>
</component>"#;
        let (engines, exec) = ibus_engines(component);
        assert_eq!(exec, None);
        assert_eq!(
            engines[0],
            InnerMethod {
                id: "hangul".to_owned(),
                name: "Hangul".to_owned(),
                icon: "ibus-hangul".to_owned(),
                label: "한".to_owned(),
                language: "ko".to_owned(),
            }
        );
        assert_eq!(engines[1].name, "English & more");
        assert_eq!(engines.len(), 2);

        let listed =
            r#"<component><name>x</name><engines exec="/bin/list --xml &amp; more" /></component>"#;
        let (engines, exec) = ibus_engines(listed);
        assert!(engines.is_empty());
        assert_eq!(exec.as_deref(), Some("/bin/list --xml & more"));
    }

    #[tokio::test]
    async fn installed_frameworks_say_what_they_have() {
        use std::os::unix::fs::PermissionsExt as _;
        let prefix = std::env::temp_dir().join(format!("omimed-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        let fcitx = prefix.join("share/fcitx5/inputmethod");
        let ibus = prefix.join("share/ibus/component");
        std::fs::create_dir_all(&fcitx).unwrap();
        std::fs::create_dir_all(&ibus).unwrap();
        std::fs::write(
            fcitx.join("shuangpin.conf"),
            "[InputMethod]\nName=Shuangpin\n",
        )
        .unwrap();
        std::fs::write(fcitx.join("keyboard-us.conf"), "[InputMethod]\nName=US\n").unwrap();
        std::fs::write(fcitx.join("notes.txt"), "[InputMethod]\nName=Notes\n").unwrap();
        // A component that lists its engines with a program, as libpinyin's does.
        let program = prefix.join("list-engines");
        std::fs::write(
            &program,
            "#!/bin/sh\necho '<engines><engine><name>libpinyin</name></engine>\
             <engine><name>shuangpin</name></engine></engines>'\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            ibus.join("libpinyin.xml"),
            format!(
                "<component><engines exec=\"{} --xml\"/></component>",
                program.display()
            ),
        )
        .unwrap();
        std::fs::write(
            ibus.join("broken.xml"),
            "<component><engines exec=\"/nonexistent\"/>",
        )
        .unwrap();

        let methods = installed(std::slice::from_ref(&prefix)).await;
        let ids: Vec<&str> = methods.iter().map(|m| m.id.as_str()).collect();
        // Keyboard layouts are left out, and the same id counts once.
        assert_eq!(ids, ["shuangpin", "libpinyin"]);
        assert!(installed(&[prefix.join("nothing")]).await.is_empty());
        let _ = std::fs::remove_dir_all(&prefix);
    }
}
