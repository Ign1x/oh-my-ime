//! Slint callbacks backed by the platform's shared translation catalog.
use crate::window::{SettingsWindow, Tr};
use omime_proto::i18n::{Translator, interpolate};
use slint::{ComponentHandle as _, SharedString};

pub fn install(window: &SettingsWindow, locale: &str) {
    let texts = Translator::for_locale(locale);
    let tr = window.global::<Tr<'_>>();
    tr.on_text(move |source| texts.text(&source).into());
    tr.on_text1(move |source, a| fill(texts.text(&source), &[&a]));
    tr.on_text2(move |source, a, b| fill(texts.text(&source), &[&a, &b]));
}

fn fill(text: &str, args: &[&str]) -> SharedString {
    let keys: Vec<_> = (0..args.len()).map(|index| index.to_string()).collect();
    let args: Vec<_> = keys
        .iter()
        .zip(args)
        .map(|(key, value)| (key.as_str(), *value))
        .collect();
    interpolate(text, &args).into()
}

#[cfg(test)]
mod tests {
    use omime_proto::i18n::{CATALOGS, parse_catalog};
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
            let texts = parse_catalog(po);
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
