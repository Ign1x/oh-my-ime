//! `omime-rime --settings`: Rime's settings window, which omimed opens on the desktop
//! from the tray menu and oh-my-ime's settings. The schemas in the user's list, the
//! candidates per page and what Shift does go into `default.custom.yaml` of the user's
//! Rime data, written by librime as its own settings programs write it; the candidate
//! window's layout, colors and font size into `rime.toml` (see [`crate::style`]).
//! omimed starts the engine again when the window closes (`restart` in its manifest),
//! and the engine deploys what changed.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle as _, Model as _, ModelRc, VecModel};

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
        // The index of what Shift does, -1 for something else.
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
                ListView {
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
                        }
                    }
                    Row {
                        Label {
                            text: Tr.text("Shift alone");
                        }
                        ComboBox {
                            enabled: root.can-edit-rime;
                            model: [Tr.text("Switches, keeping the letters typed"), Tr.text("Switches, committing the candidate"), Tr.text("Switches, clearing what was typed"), Tr.text("Does nothing")];
                            current-index <=> root.shift;
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
                            model: [Tr.text("In a row"), Tr.text("In a column")];
                            current-index <=> root.layout;
                        }
                    }
                    Row {
                        Label {
                            text: Tr.text("Appearance");
                        }
                        ComboBox {
                            model: [Tr.text("As the desktop"), Tr.text("Light"), Tr.text("Dark")];
                            current-index <=> root.appearance;
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
const SHIFT_KEYS: [&str; 2] = [
    "ascii_composer/switch_key/Shift_L",
    "ascii_composer/switch_key/Shift_R",
];
const PAGE_SIZE: &str = "menu/page_size";

/// Rime's settings as the window opened with them.
#[derive(Debug, Default)]
struct Opened {
    selected: Vec<String>,
    page_size: i32,
    shift: i32,
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

/// librime's settings, or why there are none.
fn open_levers() -> Result<Levers, String> {
    let shared = data::shared_dir()
        .ok_or_else(|| format!("No Rime data (rime-data, or ${}).", data::SHARED_DATA_ENV))?;
    let user = data::user_dir().ok_or("No home directory.")?;
    // librime reads nothing without default.custom.yaml, which the engine writes the
    // first time.
    data::customize(&shared, &user).map_err(|e| format!("{}: {e}", user.display()))?;
    Levers::open(&shared, &user)
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let window = RimeSettings::new()?;
    slint::set_xdg_app_id("io.github.ign1x.OhMyIme.Rime")?;
    window
        .global::<Tr<'_>>()
        .on_text(|source| omime_proto::i18n::text(&source).into());

    let style = Style::load();
    window.set_layout(index_of(&Layout::ALL, &style.layout));
    window.set_appearance(index_of(&Appearance::ALL, &style.appearance));
    window.set_font_size(i32::try_from(style.font_size).unwrap_or(18));

    let levers = open_levers();
    let schemas = Rc::new(VecModel::<SchemaRow>::default());
    let mut opened = Opened::default();
    match &levers {
        Ok(levers) => {
            opened.selected = levers.selected();
            opened.page_size = levers.int(PAGE_SIZE).unwrap_or(5);
            opened.shift = levers
                .string(SHIFT_KEYS[0])
                .and_then(|value| SHIFT.iter().position(|s| *s == value))
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(-1);
            schemas.extend(rows(&levers.available(), &opened.selected));
        }
        Err(e) => {
            eprintln!("omime-rime: {e}");
            let note = omime_proto::i18n::text(
                "Rime's settings cannot be read, only the candidate window's:",
            );
            window.set_problem(format!("{note} {e}").into());
        }
    }
    window.set_schemas(ModelRc::from(schemas.clone()));
    window.set_page_size(opened.page_size.max(1));
    window.set_shift(opened.shift);
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
    let note = omime_proto::i18n::text("Could not save all settings. Please retry:");
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
        return Err("Select at least one schema.".into());
    }
    // A prior attempt may have failed; rebuild its patch from the current choices.
    levers.reset()?;
    if selected != opened.selected && !levers.select(&selected) {
        return Err("Rime could not select the schemas.".into());
    }
    let page_size = window.get_page_size();
    if page_size != opened.page_size && !levers.customize_int(PAGE_SIZE, page_size) {
        return Err("Rime could not change the page size.".into());
    }
    let shift = window.get_shift();
    if shift != opened.shift
        && let Some(value) = usize::try_from(shift).ok().and_then(|i| SHIFT.get(i))
    {
        for key in SHIFT_KEYS {
            if !levers.customize_string(key, value) {
                return Err("Rime could not change the Shift key.".into());
            }
        }
    }
    levers.save().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
