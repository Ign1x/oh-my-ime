//! What the window shows, made from what omimed reports ([`Snapshot`]), and the
//! configuration that the user's changes make: no user interface and no D-Bus here.

use std::cmp::Ordering;

/// An input method, as `InputMethods` of the control API describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputMethod {
    /// What `Switch` and the `engines` list of config.toml take: `sparkle`,
    /// `fcitx5/shuangpin`.
    pub id: String,
    pub name: String,
    pub engine: String,
    pub engine_name: String,
    pub label: String,
    /// A language tag such as `zh_CN`, empty when unknown.
    pub language: String,
    pub enabled: bool,
    pub in_use: bool,
}

/// An installed engine, as `Engines` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Engine {
    pub id: String,
    pub name: String,
    /// Whether `OpenSettings` can open its settings.
    pub settings: bool,
}

/// The options of config.toml the window shows, as `Config` has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub toggle: String,
    pub next: String,
    pub key_timeout_ms: u64,
    pub remember_apps: bool,
    pub tray: bool,
    pub panel: bool,
    pub notifications: bool,
}

impl Default for Options {
    fn default() -> Self {
        let config = omime_proto::config::Config::default();
        Self {
            toggle: config.hotkeys.toggle,
            next: config.hotkeys.next,
            key_timeout_ms: config.engine.key_timeout_ms,
            remember_apps: config.apps.remember,
            tray: config.tray.enabled,
            panel: config.panel.enabled,
            notifications: config.notifications.enabled,
        }
    }
}

/// What `Diagnose` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnosis {
    /// The compositor's display.
    pub display: String,
    /// Each seat, and whether omimed is its input method.
    pub seats: Vec<(String, bool)>,
    /// Whether the compositor reports the focused application.
    pub focused_app: bool,
    /// Why the input method in use could not be selected; empty when it was.
    pub error: String,
}

/// Everything the window shows, read from omimed at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub version: String,
    /// Enabled ones first, in switching order.
    pub input_methods: Vec<InputMethod>,
    pub engines: Vec<Engine>,
    pub options: Options,
    /// Each application with `[apps] remember`, and its input method (empty: direct
    /// input).
    pub apps: Vec<(String, String)>,
    pub diagnosis: Diagnosis,
}

impl Snapshot {
    /// The enabled input methods, in switching order.
    pub fn enabled(&self) -> Vec<&InputMethod> {
        self.input_methods.iter().filter(|m| m.enabled).collect()
    }

    fn enabled_ids(&self) -> Vec<String> {
        self.enabled().iter().map(|m| m.id.clone()).collect()
    }

    /// The `engines` list with the enabled input method at `from` moved to `to`.
    pub fn moved(&self, from: usize, to: usize) -> Option<Vec<String>> {
        let mut ids = self.enabled_ids();
        if from >= ids.len() || to >= ids.len() || from == to {
            return None;
        }
        let id = ids.remove(from);
        ids.insert(to, id);
        Some(ids)
    }

    /// The `engines` list without the enabled input method at `index`.
    pub fn removed(&self, index: usize) -> Option<Vec<String>> {
        let mut ids = self.enabled_ids();
        (index < ids.len()).then(|| {
            ids.remove(index);
            ids
        })
    }

    /// The `engines` list with the input method `id` added at the end.
    pub fn added(&self, id: &str) -> Option<Vec<String>> {
        let mut ids = self.enabled_ids();
        if ids.iter().any(|i| i == id) || !self.input_methods.iter().any(|m| m.id == id) {
            return None;
        }
        ids.push(id.to_owned());
        Some(ids)
    }

    /// Whether `engine` has settings to open.
    pub fn has_settings(&self, engine: &str) -> bool {
        self.engines.iter().any(|e| e.id == engine && e.settings)
    }

    /// What an application can get, as (id, name): direct input (the empty id and
    /// name), the enabled input methods, and any input method the applications have
    /// that is not enabled any more.
    pub fn app_choices(&self) -> Vec<(String, String)> {
        let mut choices = vec![(String::new(), String::new())];
        choices.extend(
            self.enabled()
                .iter()
                .map(|m| (m.id.clone(), m.name.clone())),
        );
        for (_, id) in &self.apps {
            if !choices.iter().any(|(c, _)| c == id) {
                let name = self
                    .input_methods
                    .iter()
                    .find(|m| &m.id == id)
                    .map_or_else(|| id.clone(), |m| m.name.clone());
                choices.push((id.clone(), name));
            }
        }
        choices
    }
}

/// What an input method's badge shows: its own label, or the first letter of its name.
pub fn badge(method: &InputMethod) -> String {
    if !method.label.is_empty() {
        return method.label.clone();
    }
    method
        .name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// The engine's name, where it adds to the input method's: "Fcitx 5" for 双拼, nothing
/// for Sparkle, which is its own input method.
pub fn engine_detail(method: &InputMethod) -> String {
    if method.engine_name == method.name {
        String::new()
    } else {
        method.engine_name.clone()
    }
}

/// The group of a language tag in the list of input methods to add: `zh-Hans` and
/// `zh-Hant` for the scripts of Chinese, otherwise the language (`ja`), and empty when it
/// is not known.
pub fn language_group(tag: &str) -> String {
    let tag = tag.replace('_', "-").to_ascii_lowercase();
    let mut parts = tag.split(['-', '.', '@']);
    let language = parts.next().unwrap_or_default();
    match language {
        "" | "und" | "mul" | "zxx" | "c" | "posix" | "*" => String::new(),
        "zh" => {
            let rest: Vec<&str> = parts.collect();
            if rest
                .iter()
                .any(|p| matches!(*p, "hans" | "cn" | "sg" | "my"))
            {
                "zh-Hans".to_owned()
            } else if rest
                .iter()
                .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"))
            {
                "zh-Hant".to_owned()
            } else {
                "zh".to_owned()
            }
        }
        language => language.to_owned(),
    }
}

/// An entry of the list of input methods to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogEntry<'a> {
    /// The input methods of a language group follow.
    Heading(String),
    Method(&'a InputMethod),
}

/// The input methods that can be added, those matching `search`, by language group: the
/// group of `locale` (the user's) first, then others of the same language, then the
/// rest by name, and those of no known language last.
pub fn catalog<'a>(
    methods: &'a [InputMethod],
    search: &str,
    locale: &str,
) -> Vec<CatalogEntry<'a>> {
    let search = search.trim().to_lowercase();
    let matches = |m: &InputMethod| {
        search.is_empty()
            || [&m.name, &m.id, &m.engine_name, &m.language]
                .iter()
                .any(|text| text.to_lowercase().contains(&search))
    };
    let own = language_group(locale);
    let own_language = own.split('-').next().unwrap_or_default().to_owned();
    let rank = |group: &str| -> u8 {
        if group.is_empty() {
            3
        } else if group == own {
            0
        } else if group.split('-').next() == Some(own_language.as_str()) {
            1
        } else {
            2
        }
    };
    let mut groups: Vec<(String, Vec<&InputMethod>)> = Vec::new();
    for method in methods.iter().filter(|m| !m.enabled && matches(m)) {
        let group = language_group(&method.language);
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, members)) => members.push(method),
            None => groups.push((group, vec![method])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| match rank(a).cmp(&rank(b)) {
        Ordering::Equal => a.cmp(b),
        other => other,
    });
    let mut entries = Vec::new();
    for (group, members) in groups {
        entries.push(CatalogEntry::Heading(group));
        entries.extend(members.into_iter().map(CatalogEntry::Method));
    }
    entries
}

/// The key timeouts to choose from, in milliseconds.
pub const TIMEOUTS: [u64; 6] = [100, 200, 300, 500, 1000, 2000];

/// The key timeouts to choose from with `current` among them, and its place.
pub fn timeouts(current: u64) -> (Vec<u64>, usize) {
    let mut choices = TIMEOUTS.to_vec();
    if !choices.contains(&current) {
        choices.push(current);
        choices.sort_unstable();
    }
    let index = choices
        .iter()
        .position(|&t| t == current)
        .unwrap_or_default();
    (choices, index)
}

/// The user's locale, as the environment says it: `zh_CN.UTF-8`.
pub fn locale() -> String {
    omime_proto::i18n::locale()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn method(id: &str, name: &str, language: &str, enabled: bool) -> InputMethod {
        let engine = id.split('/').next().unwrap_or(id);
        InputMethod {
            id: id.to_owned(),
            name: name.to_owned(),
            engine: engine.to_owned(),
            engine_name: if engine == "fcitx5" { "Fcitx 5" } else { name }.to_owned(),
            language: language.to_owned(),
            enabled,
            ..InputMethod::default()
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            input_methods: vec![
                method("fcitx5/shuangpin", "双拼", "zh_CN", true),
                method("echo", "Echo", "", true),
                method("fcitx5/pinyin", "拼音", "zh_CN", true),
                method("fcitx5/cangjie", "倉頡", "zh_HK", false),
                method("fcitx5/mozc", "Mozc", "ja", false),
            ],
            ..Snapshot::default()
        }
    }

    #[test]
    fn edits_write_the_enabled_input_methods_in_their_new_order() {
        let snapshot = snapshot();
        assert_eq!(
            snapshot.moved(2, 0).unwrap(),
            ["fcitx5/pinyin", "fcitx5/shuangpin", "echo"]
        );
        assert_eq!(
            snapshot.moved(0, 1).unwrap(),
            ["echo", "fcitx5/shuangpin", "fcitx5/pinyin"]
        );
        assert_eq!(snapshot.moved(0, 3), None, "beyond the end");
        assert_eq!(
            snapshot.removed(1).unwrap(),
            ["fcitx5/shuangpin", "fcitx5/pinyin"]
        );
        assert_eq!(snapshot.removed(3), None);
        assert_eq!(
            snapshot.added("fcitx5/mozc").unwrap(),
            ["fcitx5/shuangpin", "echo", "fcitx5/pinyin", "fcitx5/mozc"]
        );
        assert_eq!(snapshot.added("echo"), None, "already enabled");
        assert_eq!(snapshot.added("nothing"), None, "not there to add");
    }

    #[test]
    fn languages_group_by_language_and_script_of_chinese() {
        assert_eq!(language_group("zh_CN"), "zh-Hans");
        assert_eq!(language_group("zh-Hans"), "zh-Hans");
        assert_eq!(language_group("zh_TW.UTF-8"), "zh-Hant");
        assert_eq!(language_group("zh_HK"), "zh-Hant");
        assert_eq!(language_group("zh"), "zh");
        assert_eq!(language_group("ja_JP"), "ja");
        assert_eq!(language_group("en"), "en");
        assert_eq!(language_group(""), "");
        assert_eq!(language_group("und"), "");
    }

    #[test]
    fn the_catalog_has_what_is_not_enabled_by_language_the_users_first() {
        let methods = snapshot().input_methods;
        let names = |entries: Vec<CatalogEntry<'_>>| -> Vec<String> {
            entries
                .into_iter()
                .map(|e| match e {
                    CatalogEntry::Heading(group) => format!("# {group}"),
                    CatalogEntry::Method(m) => m.name.clone(),
                })
                .collect()
        };
        assert_eq!(
            names(catalog(&methods, "", "zh_TW.UTF-8")),
            ["# zh-Hant", "倉頡", "# ja", "Mozc"]
        );
        assert_eq!(
            names(catalog(&methods, "", "ja_JP.UTF-8")),
            ["# ja", "Mozc", "# zh-Hant", "倉頡"]
        );
        assert_eq!(names(catalog(&methods, "MOZ", "")), ["# ja", "Mozc"]);
        assert_eq!(
            names(catalog(&methods, "fcitx", "")).len(),
            4,
            "by engine name"
        );
        assert!(catalog(&methods, "nothing like it", "").is_empty());
    }

    #[test]
    fn badges_show_the_label_or_the_first_letter() {
        let mut sparkle = method("sparkle", "sparkle", "", true);
        assert_eq!(badge(&sparkle), "S");
        sparkle.label = "✨".to_owned();
        assert_eq!(badge(&sparkle), "✨");
        assert_eq!(engine_detail(&sparkle), "", "its own input method");
        assert_eq!(
            engine_detail(&method("fcitx5/pinyin", "拼音", "", true)),
            "Fcitx 5"
        );
    }

    #[test]
    fn applications_choose_from_enabled_input_methods_and_their_own() {
        let mut snapshot = snapshot();
        snapshot.apps = vec![
            ("firefox".to_owned(), "fcitx5/mozc".to_owned()),
            ("foot".to_owned(), String::new()),
            ("kitty".to_owned(), "gone".to_owned()),
        ];
        let ids: Vec<String> = snapshot
            .app_choices()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            [
                "",
                "fcitx5/shuangpin",
                "echo",
                "fcitx5/pinyin",
                "fcitx5/mozc",
                "gone"
            ]
        );
        assert_eq!(snapshot.app_choices()[4].1, "Mozc");
    }

    #[test]
    fn the_timeout_in_use_is_always_a_choice() {
        assert_eq!(timeouts(200), (TIMEOUTS.to_vec(), 1));
        let (choices, index) = timeouts(250);
        assert_eq!(choices[index], 250);
        assert_eq!(choices.len(), TIMEOUTS.len() + 1);
    }
}
