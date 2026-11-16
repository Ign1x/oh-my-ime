//! `omime-rime --settings`: Rime's settings window, which omimed opens on the desktop
//! from the tray menu and oh-my-ime's settings. The schemas in the user's list, the
//! candidates per page and what Shift does go into `default.custom.yaml` of the user's
//! Rime data, written by librime as its own settings programs write it; the candidate
//! window's layout, colors and font size into `rime.toml` (see [`crate::style`]).
//! omimed starts the engine again when the window closes (`restart` in its manifest),
//! and the engine deploys what changed.

use std::cell::RefCell;
use std::rc::Rc;

use omime_proto::i18n;
use slint::{ComponentHandle as _, Model as _, ModelRc, SharedString, VecModel};

use crate::rime::{Available, Levers};
use crate::style::{Appearance, FONT_SIZES, Layout, Style};
use crate::{data, names};

slint::slint! {
    import { Button, CheckBox, ComboBox, ListView, Palette, SpinBox } from "std-widgets.slint";

    export struct SchemaRow {
        id: string,
        name: string,
        checked: bool,
    }

    // The colors of oh-my-ime's settings window, light or dark as the desktop prefers.
    global Theme {
        out property <bool> dark: Palette.color-scheme == ColorScheme.dark;
        out property <brush> window: self.dark ? #202020 : #f3f3f3;
        out property <brush> card: self.dark ? #2b2b2b : #ffffff;
        out property <brush> card-border: self.dark ? #ffffff10 : #0000000f;
        out property <brush> text: self.dark ? #ffffff : #1b1b1b;
        out property <brush> secondary: self.dark ? #ffffffa6 : #00000094;
        out property <brush> danger: self.dark ? #ff99a4 : #c42b1c;
    }

    component Card inherits Rectangle {
        background: Theme.card;
        border-radius: 6px;
        border-width: 1px;
        border-color: Theme.card-border;
    }

    component Heading inherits Text {
        font-size: 14px;
        font-weight: 600;
        color: Theme.text;
    }

    // The name of a setting, in a column as wide in every card.
    component Label inherits Text {
        min-width: 150px;
        color: Theme.text;
        vertical-alignment: center;
    }

    export global Tr { pure callback text(string) -> string; }

    export component RimeSettings inherits Window {
        in property <[SchemaRow]> schemas;
        // Why Rime's own settings cannot be changed, if they cannot.
        in property <string> problem;
        in property <bool> can-save: true;
        in property <bool> can-edit-rime: true;
        in-out property <int> page-size: 5;
        // What the lists offer, in the user's language. Rust sets them: a ComboBox
        // reads its model while the window is created, before `Tr` can answer, and
        // would show nothing.
        in property <[string]> shift-choices;
        in property <[string]> layout-choices;
        in property <[string]> appearance-choices;
        // The index in `shift-choices` of what Shift does.
        in-out property <int> shift: 0;
        in-out property <int> layout: 0;
        in-out property <int> appearance: 0;
        in-out property <int> font-size: 18;
        callback toggled(int, bool);
        callback save();
        callback cancel();
        callback open-folder();

        title: Tr.text("Rime Settings");
        preferred-width: 560px;
        preferred-height: 660px;
        min-width: 440px;
        min-height: 520px;
        background: Theme.window;

        VerticalLayout {
            padding: 20px;
            spacing: 10px;

            Heading {
                text: Tr.text("Schemas");
            }
            Text {
                text: Tr.text("The schemas checked are Rime's input methods; oh-my-ime's settings put them in the switching order.");
                wrap: word-wrap;
                color: Theme.secondary;
            }
            if root.problem != "": Text {
                text: root.problem;
                wrap: word-wrap;
                color: Theme.danger;
            }
            Card {
                vertical-stretch: 1;
                min-height: 160px;
                // Within the card's border and round corners.
                ListView {
                    x: 1px;
                    y: 4px;
                    width: parent.width - 2px;
                    height: parent.height - 8px;
                    for row[i] in root.schemas: HorizontalLayout {
                        padding-left: 12px;
                        padding-right: 12px;
                        padding-top: 3px;
                        padding-bottom: 3px;
                        spacing: 8px;
                        CheckBox {
                            enabled: root.can-edit-rime;
                            text: row.name;
                            checked: row.checked;
                            toggled => {
                                root.toggled(i, self.checked);
                            }
                        }
                        Text {
                            text: row.id;
                            horizontal-stretch: 1;
                            horizontal-alignment: right;
                            vertical-alignment: center;
                            font-size: 12px;
                            color: Theme.secondary;
                        }
                    }
                }
            }
            // Why Save is off: Rime needs a schema. Under the list, whose rows then stay
            // where they are.
            if !root.can-save: Text {
                text: Tr.text("Select at least one schema.");
                wrap: word-wrap;
                color: Theme.danger;
            }

            Heading {
                text: Tr.text("Typing");
            }
            Card {
                GridLayout {
                    padding: 12px;
                    spacing-horizontal: 16px;
                    spacing-vertical: 10px;
                    Row {
                        Label {
                            text: Tr.text("Candidates per page");
                        }
                        SpinBox {
                            enabled: root.can-edit-rime;
                            minimum: 1;
                            maximum: 10;
                            value <=> root.page-size;
                            accessible-label: Tr.text("Candidates per page");
                        }
                    }
                    Row {
                        Label {
                            text: Tr.text("Shift alone");
                        }
                        ComboBox {
                            enabled: root.can-edit-rime;
                            model: root.shift-choices;
                            current-index <=> root.shift;
                            accessible-label: Tr.text("Shift alone");
                        }
                    }
                }
            }

            Heading {
                text: Tr.text("Candidate window");
            }
            Card {
                GridLayout {
                    padding: 12px;
                    spacing-horizontal: 16px;
                    spacing-vertical: 10px;
                    Row {
                        Label {
                            text: Tr.text("Layout");
                        }
                        ComboBox {
                            model: root.layout-choices;
                            current-index <=> root.layout;
                            accessible-label: Tr.text("Layout");
                        }
                    }
                    Row {
                        Label {
                            text: Tr.text("Appearance");
                        }
                        ComboBox {
                            model: root.appearance-choices;
                            current-index <=> root.appearance;
                            accessible-label: Tr.text("Appearance");
                        }
                    }
                    Row {
                        Label {
                            text: Tr.text("Font size");
                        }
                        SpinBox {
                            minimum: 12;
                            maximum: 32;
                            value <=> root.font-size;
                            accessible-label: Tr.text("Font size");
                        }
                    }
                }
            }

            HorizontalLayout {
                spacing: 8px;
                Button {
                    text: Tr.text("Open the user folder");
                    clicked => {
                        root.open-folder();
                    }
                }
                Rectangle {
                    horizontal-stretch: 1;
                }
                Button {
                    text: Tr.text("Cancel");
                    clicked => {
                        root.cancel();
                    }
                }
                Button {
                    text: Tr.text("Save");
                    primary: true;
                    enabled: root.can-save;
                    clicked => {
                        root.save();
                    }
                }
            }
        }
    }
}

/// What Shift alone does (`ascii_composer/switch_key`), in the order of the window.
const SHIFT: [&str; 4] = ["commit_code", "commit_text", "clear", "noop"];
/// What the window calls the values of [`SHIFT`].
const SHIFT_CHOICES: [&str; 4] = [
    i18n::message("Switches, keeping the letters typed"),
    i18n::message("Switches, committing the candidate"),
    i18n::message("Switches, clearing what was typed"),
    i18n::message("Does nothing"),
];
/// The choice after those when Rime's files say something else, such as
/// `inline_ascii` or one thing for each Shift key: the window shows that and leaves
/// it as it is.
const SHIFT_CUSTOM: &str = i18n::message("As Rime's files say");
/// What the window calls [`Layout::ALL`] and [`Appearance::ALL`].
const LAYOUT_CHOICES: [&str; 2] = [i18n::message("In a row"), i18n::message("In a column")];
const APPEARANCE_CHOICES: [&str; 3] = [
    i18n::message("As the desktop"),
    i18n::message("Light"),
    i18n::message("Dark"),
];
const SHIFT_KEYS: [&str; 2] = [
    "ascii_composer/switch_key/Shift_L",
    "ascii_composer/switch_key/Shift_R",
];
const PAGE_SIZE: &str = "menu/page_size";

/// Rime's settings as the window opened with them.
#[derive(Debug)]
struct Opened {
    selected: Vec<String>,
    page_size: i32,
    /// The index in the window's list of what Shift does.
    shift: usize,
}

impl Default for Opened {
    /// What the window shows of Rime's settings when it cannot read them.
    fn default() -> Self {
        Self {
            selected: Vec::new(),
            page_size: 5,
            shift: SHIFT.len(),
        }
    }
}

/// The rows of the list: the user's schemas first, in their order, then the others in
/// librime's.
fn rows(available: &[Available], selected: &[String]) -> Vec<SchemaRow> {
    let row = |schema: &Available, checked: bool| SchemaRow {
        id: schema.id.as_str().into(),
        name: names::display(&schema.name).into(),
        checked,
    };
    let mut rows: Vec<SchemaRow> = selected
        .iter()
        .map(|id| {
            available.iter().find(|s| &s.id == id).map_or_else(
                || SchemaRow {
                    id: id.as_str().into(),
                    name: id.as_str().into(),
                    checked: true,
                },
                |s| row(s, true),
            )
        })
        .collect();
    rows.extend(
        available
            .iter()
            .filter(|s| !selected.contains(&s.id))
            .map(|s| row(s, false)),
    );
    rows
}

fn index_of<T: PartialEq>(all: &[T], value: &T) -> i32 {
    all.iter()
        .position(|v| v == value)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// The index in the window's list of what Rime's files make the two Shift keys do:
/// in [`SHIFT`] when both do the same and the window offers it, after it
/// ([`SHIFT_CUSTOM`]) otherwise.
fn shift_index(left: Option<&str>, right: Option<&str>) -> usize {
    left.filter(|_| left == right)
        .and_then(|value| SHIFT.iter().position(|s| *s == value))
        .unwrap_or(SHIFT.len())
}

/// The choices of a list, in the user's language.
fn choices<'a>(sources: impl IntoIterator<Item = &'a str>) -> ModelRc<SharedString> {
    let texts: Vec<SharedString> = sources
        .into_iter()
        .map(|source| i18n::text(source).into())
        .collect();
    ModelRc::from(Rc::new(VecModel::from(texts)))
}

/// Puts the settings into the window, in the user's language: `Tr` answers the
/// bindings, and the lists get their choices (see `shift-choices`) before their
/// indexes, as a list keeps an index only within its choices.
fn present(window: &RimeSettings, style: &Style, opened: &Opened) {
    window
        .global::<Tr<'_>>()
        .on_text(|source| i18n::text(&source).into());
    let custom_shift = opened.shift >= SHIFT.len();
    window.set_shift_choices(choices(
        SHIFT_CHOICES
            .into_iter()
            .chain(custom_shift.then_some(SHIFT_CUSTOM)),
    ));
    window.set_layout_choices(choices(LAYOUT_CHOICES));
    window.set_appearance_choices(choices(APPEARANCE_CHOICES));
    window.set_shift(i32::try_from(opened.shift).unwrap_or(0));
    window.set_layout(index_of(&Layout::ALL, &style.layout));
    window.set_appearance(index_of(&Appearance::ALL, &style.appearance));
    window.set_font_size(i32::try_from(style.font_size).unwrap_or(18));
    window.set_page_size(opened.page_size.max(1));
}

/// librime's settings, or why there are none.
fn open_levers() -> Result<Levers, String> {
    let shared = data::shared_dir().ok_or_else(|| {
        omime_proto::tr!(
            "No Rime data (the rime-data package, or ${variable}).",
            variable = data::SHARED_DATA_ENV
        )
    })?;
    let user = data::user_dir().ok_or_else(|| i18n::text("No home directory.").to_owned())?;
    // librime reads nothing without default.custom.yaml, which the engine writes the
    // first time.
    data::customize(&shared, &user).map_err(|e| format!("{}: {e}", user.display()))?;
    Levers::open(&shared, &user)
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let window = RimeSettings::new()?;
    slint::set_xdg_app_id("io.github.ign1x.OhMyIme.Rime")?;

    let style = Style::load();
    let levers = open_levers();
    let schemas = Rc::new(VecModel::<SchemaRow>::default());
    let mut opened = Opened::default();
    match &levers {
        Ok(levers) => {
            opened.selected = levers.selected();
            opened.page_size = levers.int(PAGE_SIZE).unwrap_or(5);
            let [left, right] = SHIFT_KEYS.map(|key| levers.string(key));
            opened.shift = shift_index(left.as_deref(), right.as_deref());
            schemas.extend(rows(&levers.available(), &opened.selected));
        }
        Err(e) => {
            eprintln!("omime-rime: {e}");
            let note = i18n::text("Rime's settings cannot be read, only the candidate window's:");
            window.set_problem(format!("{note} {e}").into());
        }
    }
    present(&window, &style, &opened);
    window.set_schemas(ModelRc::from(schemas.clone()));
    window.set_can_edit_rime(levers.is_ok());
    window.set_can_save(levers.is_err() || schemas.iter().any(|row| row.checked));

    let weak = window.as_weak();
    let rows_toggled = schemas.clone();
    let levers_ok = levers.is_ok();
    window.on_toggled(move |i, checked| {
        let Ok(i) = usize::try_from(i) else { return };
        if let Some(mut row) = rows_toggled.row_data(i) {
            row.checked = checked;
            rows_toggled.set_row_data(i, row);
        }
        // Rime needs a schema.
        let any = rows_toggled.iter().any(|row| row.checked);
        if let Some(window) = weak.upgrade() {
            window.set_can_save(any || !levers_ok);
        }
    });

    let weak = window.as_weak();
    let levers = Rc::new(levers);
    let saved_style = RefCell::new(style);
    window.on_save(move || {
        let Some(window) = weak.upgrade() else { return };
        let saved = Style {
            layout: usize::try_from(window.get_layout())
                .ok()
                .and_then(|i| Layout::ALL.get(i).copied())
                .unwrap_or_default(),
            appearance: usize::try_from(window.get_appearance())
                .ok()
                .and_then(|i| Appearance::ALL.get(i).copied())
                .unwrap_or_default(),
            font_size: u32::try_from(window.get_font_size())
                .unwrap_or(18)
                .clamp(*FONT_SIZES.start(), *FONT_SIZES.end()),
        };
        if saved != *saved_style.borrow()
            && let Err(e) = saved.save()
        {
            save_error(&window, &e.to_string());
            return;
        }
        *saved_style.borrow_mut() = saved;
        if let Ok(levers) = &*levers
            && let Err(e) = save_rime(levers, &opened, &window, &schemas)
        {
            save_error(&window, &e);
            return;
        }
        let _ = slint::quit_event_loop();
    });
    window.on_cancel(|| {
        let _ = slint::quit_event_loop();
    });
    window.on_open_folder(|| {
        let Some(user) = data::user_dir() else { return };
        if let Err(e) = std::process::Command::new("xdg-open").arg(&user).spawn() {
            eprintln!("omime-rime: cannot open {}: {e}", user.display());
        }
    });

    window.run()?;
    Ok(())
}

fn save_error(window: &RimeSettings, error: &str) {
    eprintln!("omime-rime: cannot save settings: {error}");
    let note = i18n::text("Could not save all settings. Please retry:");
    window.set_problem(format!("{note} {error}").into());
}

/// Writes what changed of Rime's own settings into default.custom.yaml.
fn save_rime(
    levers: &Levers,
    opened: &Opened,
    window: &RimeSettings,
    schemas: &VecModel<SchemaRow>,
) -> Result<(), String> {
    let selected: Vec<String> = schemas
        .iter()
        .filter(|row| row.checked)
        .map(|row| row.id.to_string())
        .collect();
    if selected.is_empty() {
        return Err(i18n::text("Select at least one schema.").into());
    }
    // A prior attempt may have failed; rebuild its patch from the current choices.
    levers.reset()?;
    if selected != opened.selected && !levers.select(&selected) {
        return Err(i18n::text("Rime could not select the schemas.").into());
    }
    let page_size = window.get_page_size();
    if page_size != opened.page_size && !levers.customize_int(PAGE_SIZE, page_size) {
        return Err(i18n::text("Rime could not change the page size.").into());
    }
    // The choice after `SHIFT` is what Rime's files say, which stays.
    let shift = usize::try_from(window.get_shift()).unwrap_or(opened.shift);
    if shift != opened.shift
        && let Some(value) = SHIFT.get(shift)
    {
        for key in SHIFT_KEYS {
            if !levers.customize_string(key, value) {
                return Err(i18n::text("Rime could not change the Shift key.").into());
            }
        }
    }
    levers.save().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_is_a_choice_of_the_window_when_both_keys_do_it() {
        assert_eq!(shift_index(Some("commit_code"), Some("commit_code")), 0);
        assert_eq!(shift_index(Some("noop"), Some("noop")), 3);
        // Rime's own defaults, something the window does not offer, and nothing
        // deployed yet.
        for (left, right) in [
            (Some("inline_ascii"), Some("commit_text")),
            (Some("commit_code"), Some("commit_text")),
            (Some("inline_ascii"), Some("inline_ascii")),
            (Some("commit_code"), None),
            (None, None),
        ] {
            assert_eq!(shift_index(left, right), SHIFT.len(), "{left:?} {right:?}");
        }
    }

    #[test]
    fn the_users_schemas_come_first_in_their_order() {
        let available: Vec<Available> = ["luna_pinyin", "cangjie5", "double_pinyin_flypy"]
            .iter()
            .map(|id| Available {
                id: (*id).to_owned(),
                name: if *id == "double_pinyin_flypy" {
                    "小鶴雙拼"
                } else {
                    "x"
                }
                .to_owned(),
            })
            .collect();
        let selected = [
            "double_pinyin_flypy".to_owned(),
            "luna_pinyin".to_owned(),
            "gone".to_owned(),
        ];
        let rows = rows(&available, &selected);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["double_pinyin_flypy", "luna_pinyin", "gone", "cangjie5"]
        );
        let checked: Vec<bool> = rows.iter().map(|r| r.checked).collect();
        assert_eq!(checked, [true, true, true, false]);
    }

    /// The window itself, without a display.
    #[cfg(slint_debug_info)]
    mod window {
        use i_slint_backend_testing::{AccessibleRole, ElementHandle};

        use super::*;

        /// The window as [`run`] presents it, once the event loop has turned: the
        /// lists have looked at their choices by then.
        fn open(style: &Style, opened: &Opened) -> RimeSettings {
            i_slint_backend_testing::init_no_event_loop();
            let window = RimeSettings::new().expect("a window");
            present(&window, style, opened);
            window.show().expect("shown");
            slint::platform::update_timers_and_animations();
            window
        }

        /// What the list named `label` shows.
        fn shown(window: &RimeSettings, label: &str) -> String {
            let lists: Vec<ElementHandle> =
                ElementHandle::find_by_accessible_label(window, i18n::text(label))
                    .filter(|e| e.accessible_role() == Some(AccessibleRole::Combobox))
                    .collect();
            assert_eq!(lists.len(), 1, "lists named {label:?}");
            lists[0]
                .accessible_value()
                .map(|value| value.to_string())
                .unwrap_or_default()
        }

        #[test]
        fn the_lists_show_what_is_set() {
            let style = Style {
                layout: Layout::Vertical,
                appearance: Appearance::Dark,
                font_size: 20,
            };
            let opened = Opened {
                shift: 1,
                ..Opened::default()
            };
            let window = open(&style, &opened);
            assert_eq!(
                shown(&window, "Shift alone"),
                i18n::text("Switches, committing the candidate")
            );
            assert_eq!(shown(&window, "Layout"), i18n::text("In a column"));
            assert_eq!(shown(&window, "Appearance"), i18n::text("Dark"));
            assert_eq!(window.get_font_size(), 20);
        }

        #[test]
        fn what_rimes_files_say_of_shift_is_shown_and_stays() {
            let opened = Opened {
                shift: shift_index(Some("inline_ascii"), Some("commit_text")),
                ..Opened::default()
            };
            let window = open(&Style::default(), &opened);
            assert_eq!(shown(&window, "Shift alone"), i18n::text(SHIFT_CUSTOM));
            // Not one of the window's own choices, which saving would write.
            let shift = usize::try_from(window.get_shift()).expect("an index");
            assert_eq!(shift, opened.shift);
            assert_eq!(SHIFT.get(shift), None);
        }

        #[test]
        fn the_window_says_why_it_cannot_save() {
            let window = open(&Style::default(), &Opened::default());
            let hints = |window: &RimeSettings| {
                let hint = i18n::text("Select at least one schema.");
                ElementHandle::find_by_accessible_label(window, hint).count()
            };
            assert_eq!(hints(&window), 0);
            window.set_can_save(false);
            assert_eq!(hints(&window), 1);
        }

        #[test]
        fn the_window_offers_only_its_choices_for_a_shift_key_it_knows() {
            let opened = Opened {
                shift: 0,
                ..Opened::default()
            };
            let window = open(&Style::default(), &opened);
            assert_eq!(window.get_shift_choices().row_count(), SHIFT.len());
            assert_eq!(
                shown(&window, "Shift alone"),
                i18n::text("Switches, keeping the letters typed")
            );
        }
    }
}
