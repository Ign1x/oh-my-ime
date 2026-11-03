//! The window's texts in the user's language: `Tr.text("Input methods")` in the user
//! interface is looked up in `po/<language>.po`, which is built in; English, the
//! language of the user interface itself, needs no file.

use std::collections::HashMap;
use std::rc::Rc;

use slint::{ComponentHandle as _, SharedString};

use crate::window::{SettingsWindow, Tr};

/// The translations there are, by language.
const CATALOGS: [(&str, &str); 1] = [("zh", include_str!("../po/zh_CN.po"))];

/// Gives the window the texts in the language of `locale` (`zh_CN.UTF-8`).
pub fn install(window: &SettingsWindow, locale: &str) {
    let language = locale
        .split(['_', '.', '@', '-'])
        .next()
        .unwrap_or_default();
    let texts: Rc<HashMap<String, String>> = Rc::new(
        CATALOGS
            .iter()
            .find(|(l, _)| *l == language)
            .map(|(_, po)| parse(po))
            .unwrap_or_default(),
    );
    let tr = window.global::<Tr<'_>>();
    let lookup = {
        let texts = texts.clone();
        move |text: &str| -> String { texts.get(text).cloned().unwrap_or_else(|| text.to_owned()) }
    };
    let text = lookup.clone();
    tr.on_text(move |source| text(&source).into());
    let text = lookup.clone();
    tr.on_text1(move |source, a| fill(&text(&source), &[&a]));
    tr.on_text2(move |source, a, b| fill(&lookup(&source), &[&a, &b]));
}

/// `text` with each `{}` replaced by the next of `args`.
fn fill(text: &str, args: &[&str]) -> SharedString {
    let mut out = String::with_capacity(text.len());
    let mut args = args.iter();
    let mut rest = text;
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        out.push_str(args.next().copied().unwrap_or_default());
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out.into()
}

/// The translations of a gettext catalog (`msgid` → `msgstr`), without the header and
/// without untranslated entries.
fn parse(po: &str) -> HashMap<String, String> {
    #[derive(PartialEq)]
    enum Field {
        None,
        Id,
        Text,
    }
    let mut texts = HashMap::new();
    let (mut id, mut text, mut field) = (String::new(), String::new(), Field::None);
    let mut finish = |id: &mut String, text: &mut String| {
        if !id.is_empty() && !text.is_empty() {
            texts.insert(std::mem::take(id), std::mem::take(text));
        }
        id.clear();
        text.clear();
    };
    for line in po.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("msgid ") {
            finish(&mut id, &mut text);
            id = unquote(rest);
            field = Field::Id;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            text = unquote(rest);
            field = Field::Text;
        } else if line.starts_with('"') {
            match field {
                Field::Id => id.push_str(&unquote(line)),
                Field::Text => text.push_str(&unquote(line)),
                Field::None => {}
            }
        }
    }
    finish(&mut id, &mut text);
    texts
}

/// The text of a quoted gettext string.
fn unquote(quoted: &str) -> String {
    let inner = quoted
        .trim()
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
    fn catalogs_read_as_their_translations() {
        let po = "# A comment\nmsgid \"\"\nmsgstr \"\"\n\"Language: zh_CN\\n\"\n\n\
                  msgid \"Say \\\"{}\\\"\"\nmsgstr \"说“{}”\"\n\n\
                  msgid \"Long\"\n\"er\"\nmsgstr \"\"\n\"长\"\n\n\
                  msgid \"Untranslated\"\nmsgstr \"\"\n";
        let texts = parse(po);
        assert_eq!(texts.get("Say \"{}\"").map(String::as_str), Some("说“{}”"));
        assert_eq!(texts.get("Longer").map(String::as_str), Some("长"));
        assert_eq!(texts.len(), 2, "no header, nothing untranslated: {texts:?}");
        assert_eq!(fill("{} and {}", &["a", "b"]), "a and b");
        assert_eq!(fill("{} only", &[]), " only");
    }

    /// Every text of the user interface has its translation.
    #[test]
    fn every_text_is_translated() {
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let mut files = vec![ui.clone(), ui.join("pages")]
            .into_iter()
            .flat_map(|dir| std::fs::read_dir(dir).expect("ui/").flatten())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "slint"))
            .collect::<Vec<_>>();
        files.sort();
        for (language, po) in CATALOGS {
            let texts = parse(po);
            let mut missing = Vec::new();
            let mut seen = 0;
            for file in &files {
                let source = std::fs::read_to_string(file).expect("a .slint file");
                for call in ["Tr.text(\"", "Tr.text1(\"", "Tr.text2(\""] {
                    for (at, _) in source.match_indices(call) {
                        let rest = &source[at + call.len()..];
                        let end = rest.find('"').expect("a closing quote");
                        let text = &rest[..end];
                        seen += 1;
                        if !texts.contains_key(text) {
                            missing.push(text.to_owned());
                        }
                    }
                }
            }
            assert!(seen > 50, "found only {seen} texts");
            assert!(missing.is_empty(), "missing in {language}: {missing:#?}");
        }
    }
}
