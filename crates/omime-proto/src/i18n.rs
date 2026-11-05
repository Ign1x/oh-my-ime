//! Shared UI translations. Catalogs are embedded, so installed binaries and engines
//! agree without a system gettext installation or generated locales.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Catalogs use English source text as keys. Add a language here and its PO file.
pub const CATALOGS: &[(&str, &str)] = &[("zh", include_str!("../po/zh_CN.po"))];

/// Normalize a POSIX locale or BCP 47 tag, without encoding or modifier suffixes.
pub fn normalize(locale: &str) -> String {
    locale
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-")
        .to_ascii_lowercase()
}

/// Most specific translations first, then the language. Script tags for simplified
/// Chinese also find the conventional zh_CN catalog used by engine manifests.
pub fn candidates(locale: &str) -> Vec<String> {
    let tag = normalize(locale);
    let mut result = vec![tag.clone()];
    let parts: Vec<_> = tag.split('-').collect();
    if parts.len() > 2 {
        result.push(format!("{}-{}", parts[0], parts[parts.len() - 1]));
        result.push(parts[..parts.len() - 1].join("-"));
    }
    if parts[0] == "zh" && (parts.contains(&"hans") || tag == "zh-sg" || tag == "zh") {
        result.push("zh-cn".into());
    }
    result.push(parts[0].to_owned());
    let mut seen = std::collections::HashSet::new();
    result.retain(|tag| !tag.is_empty() && seen.insert(tag.clone()));
    result
}

/// Language preferences for UI text. Empty environment variables are ignored.
/// LANGUAGE supplies an ordered fallback list unless the message locale is C/POSIX.
pub fn preferences() -> Vec<String> {
    preferences_with(|key| std::env::var(key).ok())
}

fn preferences_with(mut get: impl FnMut(&str) -> Option<String>) -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(&mut get)
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "C".into());
    if matches!(normalize(&locale).as_str(), "c" | "posix") {
        return vec!["en".into()];
    }
    let mut languages: Vec<_> = get("LANGUAGE")
        .unwrap_or_default()
        .split(':')
        .filter(|part| !part.trim().is_empty())
        .map(str::to_owned)
        .collect();
    languages.push(locale);
    languages
}

/// The first preferred language, for sorting input methods and native engine names.
pub fn locale() -> String {
    preferences().into_iter().next().unwrap_or_default()
}

#[derive(Debug, Clone, Copy)]
pub struct Translator {
    language: &'static str,
    texts: Option<&'static HashMap<String, String>>,
}

impl Translator {
    pub fn for_locale(locale: &str) -> Self {
        Self::for_languages(locale.split(':'))
    }

    pub fn for_languages<'a>(languages: impl IntoIterator<Item = &'a str>) -> Self {
        static TEXTS: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
        let texts =
            TEXTS.get_or_init(|| CATALOGS.iter().map(|(_, po)| parse_catalog(po)).collect());
        for locale in languages {
            for candidate in candidates(locale) {
                if matches!(candidate.as_str(), "en" | "c" | "posix") {
                    return Self {
                        language: "en",
                        texts: None,
                    };
                }
                if let Some(at) = CATALOGS
                    .iter()
                    .position(|(tag, _)| normalize(tag) == candidate)
                {
                    return Self {
                        language: CATALOGS[at].0,
                        texts: Some(&texts[at]),
                    };
                }
            }
        }
        Self {
            language: "en",
            texts: None,
        }
    }

    pub fn language(&self) -> &'static str {
        self.language
    }

    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        self.texts
            .and_then(|texts| texts.get(source))
            .map_or(source, String::as_str)
    }

    pub fn format(&self, source: &str, args: &[(&str, &str)]) -> String {
        interpolate(self.text(source), args)
    }
}

/// Process-wide translator. Locale changes take effect when the process restarts.
pub fn current() -> &'static Translator {
    static CURRENT: OnceLock<Translator> = OnceLock::new();
    CURRENT.get_or_init(|| Translator::for_languages(preferences().iter().map(String::as_str)))
}

pub fn text(source: &str) -> &str {
    current().text(source)
}

/// Mark a message chosen before translation (for example, a match arm or constant).
pub const fn message(source: &str) -> &str {
    source
}

/// Named placeholders can be reordered in translations; inserted values are never
/// interpreted as templates. Unfilled placeholders remain visible.
pub fn interpolate(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut position = 0;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("{{") {
            out.push('{');
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("}}") {
            out.push('}');
            rest = tail;
        } else if rest.starts_with('{')
            && let Some(end) = rest.find('}')
        {
            let name = &rest[1..end];
            let index = position.to_string();
            let name = if name.is_empty() {
                position += 1;
                &index
            } else {
                name
            };
            if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                out.push_str(value);
            } else {
                out.push_str(&rest[..=end]);
            }
            rest = &rest[end + 1..];
        } else {
            let length = rest.chars().next().map_or(0, char::len_utf8);
            out.push_str(&rest[..length]);
            rest = &rest[length..];
        }
    }
    out
}

/// Translate a complete sentence with named values, e.g.
/// `tr!("Configure {engine}…", engine = "Rime")`.
#[macro_export]
macro_rules! tr {
    ($source:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::current().format($source, &[$((stringify!($name), &($value).to_string())),*])
    };
}

/// Read the singular gettext entries used by the platform. Fuzzy, obsolete,
/// contextual and plural entries are excluded rather than misinterpreted.
pub fn parse_catalog(po: &str) -> HashMap<String, String> {
    let mut texts = HashMap::new();
    for entry in po.split("\n\n") {
        if entry.lines().any(|line| {
            line.starts_with("#, ") && line.split([',', ' ']).any(|word| word == "fuzzy")
                || line.starts_with("msgctxt ")
                || line.starts_with("msgid_plural ")
        }) {
            continue;
        }
        let (mut id, mut value) = (String::new(), String::new());
        let mut field = None;
        for line in entry.lines().map(str::trim) {
            if let Some(quoted) = line.strip_prefix("msgid ") {
                id = unquote(quoted);
                field = Some(false);
            } else if let Some(quoted) = line.strip_prefix("msgstr ") {
                value = unquote(quoted);
                field = Some(true);
            } else if line.starts_with('"') {
                match field {
                    Some(false) => id.push_str(&unquote(line)),
                    Some(true) => value.push_str(&unquote(line)),
                    None => {}
                }
            }
        }
        if !id.is_empty() && !value.is_empty() {
            texts.insert(id, value);
        }
    }
    texts
}

fn unquote(quoted: &str) -> String {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_default();
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_precedence_and_language_fallback() {
        let preferences = |vars: &[(&str, &str)]| {
            preferences_with(|key| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(
            preferences(&[
                ("LC_ALL", ""),
                ("LC_MESSAGES", "zh_CN.UTF-8"),
                ("LANG", "en_US.UTF-8")
            ]),
            ["zh_CN.UTF-8"]
        );
        assert_eq!(
            preferences(&[
                ("LC_ALL", "C"),
                ("LANGUAGE", "zh_CN"),
                ("LANG", "zh_CN.UTF-8")
            ]),
            ["en"]
        );
        assert_eq!(
            preferences(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "fr:zh_CN:en")]),
            ["fr", "zh_CN", "en", "en_US.UTF-8"]
        );
        for locale in ["zh_CN.UTF-8", "zh-Hans", "ZH_cn@pinyin", "fr:zh_CN:en"] {
            assert_eq!(
                Translator::for_locale(locale).text("Direct input"),
                "直接输入"
            );
        }
        assert_eq!(
            Translator::for_locale("en:zh").text("Direct input"),
            "Direct input"
        );
        assert_eq!(
            Translator::for_locale("de_DE").text("Direct input"),
            "Direct input"
        );
        assert_eq!(
            Translator::for_locale("zh").text("Untranslated"),
            "Untranslated"
        );
    }

    #[test]
    fn placeholders_preserve_values_and_can_change_order() {
        assert_eq!(
            interpolate(
                "{second} / {first}",
                &[("first", "{second}"), ("second", "你好")]
            ),
            "你好 / {second}"
        );
        assert_eq!(
            interpolate("{{{name}}} {missing}", &[("name", "Rime")]),
            "{Rime} {missing}"
        );
        assert_eq!(
            interpolate("{} and {}", &[("0", "a"), ("1", "b")]),
            "a and b"
        );
    }

    #[test]
    fn translations_preserve_every_placeholder() {
        fn parameters(text: &str) -> Vec<String> {
            let mut result = Vec::new();
            let mut rest = text;
            let mut positional = 0;
            while let Some(at) = rest.find('{') {
                rest = &rest[at + 1..];
                if let Some(tail) = rest.strip_prefix('{') {
                    rest = tail;
                    continue;
                }
                let end = rest.find('}').expect("closed placeholder");
                let key = &rest[..end];
                result.push(if key.is_empty() {
                    positional += 1;
                    (positional - 1).to_string()
                } else {
                    key.to_owned()
                });
                rest = &rest[end + 1..];
            }
            result.sort();
            result
        }
        for (language, po) in CATALOGS {
            let catalog = parse_catalog(po);
            assert!(catalog.len() > 300);
            for (source, translation) in catalog {
                assert_eq!(
                    parameters(&source),
                    parameters(&translation),
                    "{language}: {source}"
                );
            }
        }
    }

    #[test]
    fn catalogs_ignore_drafts_and_read_escaped_multiline_strings() {
        let po = "msgid \"\"\nmsgstr \"\"\n\"Language: zh_CN\\n\"\n\nmsgid \"Say \\\"{}\\\"\"\nmsgstr \"说“{}”\"\n\nmsgid \"Long\"\n\"er\"\nmsgstr \"\"\n\"长\"\n\n#, fuzzy\nmsgid \"Draft\"\nmsgstr \"草稿\"\n\n#~ msgid \"Old\"\n#~ msgstr \"旧\"\n";
        let texts = parse_catalog(po);
        assert_eq!(texts.get("Say \"{}\"").map(String::as_str), Some("说“{}”"));
        assert_eq!(texts.get("Longer").map(String::as_str), Some("长"));
        assert_eq!(texts.len(), 2);
    }
}
