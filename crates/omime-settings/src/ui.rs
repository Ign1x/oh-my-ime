//! The window: shows what omimed has ([`Snapshot`]) and turns what the user does into
//! [`Command`]s for omimed.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use omime_proto::config::Value;
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::daemon::{Command, Event};
use crate::hotkey::{self, Recorded};
use crate::model::{self, Snapshot};
use crate::window::{AppRow, Backend, CatalogEntry, Check, InputMethodRow, SettingsWindow, Texts};

/// The pages, by the names `omime-settings` takes and in the order of the sidebar.
pub const PAGES: [&str; 5] = [
    "input-methods",
    "hotkeys",
    "applications",
    "general",
    "about",
];

/// Fills the window and answers its callbacks.
pub struct Controller {
    window: slint::Weak<SettingsWindow>,
    commands: UnboundedSender<Command>,
    snapshot: RefCell<Option<Snapshot>>,
    search: RefCell<String>,
    /// The ids of `app-choices`.
    app_choices: RefCell<Vec<String>>,
    /// The milliseconds of `timeouts`.
    timeouts: RefCell<Vec<u64>>,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

/// Runs `f` on the controller while it exists.
fn with<A>(this: &Weak<Controller>, args: A, f: impl FnOnce(&Controller, A)) {
    if let Some(this) = this.upgrade() {
        f(&this, args);
    }
}

impl Controller {
    pub fn new(window: &SettingsWindow, commands: UnboundedSender<Command>) -> Rc<Self> {
        let this = Rc::new(Self {
            window: window.as_weak(),
            commands,
            snapshot: RefCell::new(None),
            search: RefCell::new(String::new()),
            app_choices: RefCell::new(Vec::new()),
            timeouts: RefCell::new(Vec::new()),
        });
        let backend = window.global::<Backend<'_>>();
        backend.set_version(env!("CARGO_PKG_VERSION").into());

        let weak = Rc::downgrade(&this);
        backend.on_dismiss_error(move || {
            with(&weak, (), |this, ()| {
                this.backend(|b| b.set_error("".into()))
            })
        });
        let weak = Rc::downgrade(&this);
        backend.on_search(move |text| with(&weak, text, Self::search));
        let weak = Rc::downgrade(&this);
        backend.on_move_input_method(move |from, to| {
            with(&weak, (from, to), |this, (from, to)| {
                let engines = this.edit(|s| s.moved(index(from)?, index(to)?));
                this.set_engines(engines);
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_remove_input_method(move |at| {
            with(&weak, at, |this, at| {
                let engines = this.edit(|s| s.removed(index(at)?));
                this.set_engines(engines);
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_add_input_method(move |id| {
            with(&weak, id, |this, id| {
                let engines = this.edit(|s| s.added(&id));
                this.set_engines(engines);
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_open_settings(move |engine| {
            with(&weak, engine, |this, engine| {
                this.send(Command::OpenSettings(engine.into()));
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_record_key(move |which, text, control, alt, shift, meta| {
            let mut done = false;
            with(&weak, (), |this, ()| {
                done = match hotkey::record(&text, control, alt, shift, meta) {
                    Recorded::Pending => false,
                    Recorded::Cancelled => true,
                    Recorded::Hotkey(hotkey) => {
                        this.configure(&format!("hotkeys.{which}"), Value::String(hotkey));
                        true
                    }
                };
            });
            done
        });
        let weak = Rc::downgrade(&this);
        backend.on_clear_hotkey(move |which| {
            with(&weak, which, |this, which| {
                this.configure(&format!("hotkeys.{which}"), Value::String(String::new()));
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_remember_apps(move |on| {
            with(&weak, on, |this, on| {
                this.configure("apps.remember", Value::Bool(on))
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_app_input_method(move |app, choice| {
            with(&weak, (app, choice), |this, (app, choice)| {
                let id = index(choice).and_then(|i| this.app_choices.borrow().get(i).cloned());
                if let Some(id) = id {
                    this.send(Command::SetAppInputMethod(app.into(), id));
                }
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_forget_app(move |app| {
            with(&weak, app, |this, app| {
                this.send(Command::ForgetApp(app.into()))
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_tray(move |on| {
            with(&weak, on, |this, on| {
                this.configure("tray.enabled", Value::Bool(on))
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_switcher(move |on| {
            with(&weak, on, |this, on| {
                this.configure("panel.enabled", Value::Bool(on))
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_notifications(move |on| {
            with(&weak, on, |this, on| {
                this.configure("notifications.enabled", Value::Bool(on))
            });
        });
        let weak = Rc::downgrade(&this);
        backend.on_set_timeout(move |choice| {
            with(&weak, choice, |this, choice| {
                let ms = index(choice).and_then(|i| this.timeouts.borrow().get(i).copied());
                if let Some(ms) = ms.and_then(|ms| i64::try_from(ms).ok()) {
                    this.configure("engine.key_timeout_ms", Value::Integer(ms));
                }
            });
        });
        this
    }

    /// Shows what the D-Bus thread learned.
    pub fn apply(&self, event: Event) {
        match event {
            Event::Snapshot(snapshot) => {
                self.show(&snapshot);
                self.snapshot.replace(Some(*snapshot));
                self.backend(|b| {
                    b.set_loaded(true);
                    b.set_connected(true);
                });
            }
            Event::Absent => {
                self.snapshot.replace(None);
                self.backend(|b| {
                    b.set_loaded(true);
                    b.set_connected(false);
                    b.set_error("".into());
                });
            }
            Event::Failed(message) => self.backend(|b| b.set_error(message.into())),
        }
    }

    fn backend(&self, f: impl FnOnce(Backend<'_>)) {
        if let Some(window) = self.window.upgrade() {
            f(window.global::<Backend<'_>>());
        }
    }

    fn send(&self, command: Command) {
        debug!(?command, "the user asks");
        // The D-Bus thread only stops with the window.
        let _ = self.commands.send(command);
    }

    fn configure(&self, key: &str, value: Value) {
        self.send(Command::Configure(vec![(key.to_owned(), value)]));
    }

    /// The `engines` list `f` makes from the snapshot, if any.
    fn edit(&self, f: impl FnOnce(&Snapshot) -> Option<Vec<String>>) -> Option<Vec<String>> {
        self.snapshot.borrow().as_ref().and_then(f)
    }

    fn set_engines(&self, engines: Option<Vec<String>>) {
        if let Some(engines) = engines {
            self.configure("engines", Value::List(engines));
        }
    }

    fn search(&self, text: SharedString) {
        self.search.replace(text.into());
        if let Some(snapshot) = &*self.snapshot.borrow() {
            self.show_catalog(snapshot);
        }
    }

    fn show(&self, snapshot: &Snapshot) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let backend = window.global::<Backend<'_>>();
        let rows: Vec<InputMethodRow> = snapshot
            .enabled()
            .into_iter()
            .map(|m| InputMethodRow {
                id: m.id.as_str().into(),
                name: m.name.as_str().into(),
                engine_name: model::engine_detail(m).into(),
                language: model::language_group(&m.language).into(),
                label: model::badge(m).into(),
                in_use: m.in_use,
                engine: m.engine.as_str().into(),
                settings: snapshot.has_settings(&m.engine),
            })
            .collect();
        backend.set_input_methods(model_of(rows));
        self.show_catalog(snapshot);

        let options = &snapshot.options;
        backend.set_toggle_keys(model_of(strings(hotkey::keys(&options.toggle))));
        backend.set_next_keys(model_of(strings(hotkey::keys(&options.next))));
        backend.set_remember_apps(options.remember_apps);
        backend.set_tray(options.tray);
        backend.set_switcher(options.panel);
        backend.set_notifications(options.notifications);
        let (timeouts, current) = model::timeouts(options.key_timeout_ms);
        let labels: Vec<SharedString> = timeouts.iter().map(|&ms| timeout_label(ms)).collect();
        backend.set_timeouts(model_of(labels));
        backend.set_timeout_index(i32::try_from(current).unwrap_or_default());
        self.timeouts.replace(timeouts);

        let choices = snapshot.app_choices();
        let direct_input = window.global::<Texts<'_>>().get_direct_input();
        let names: Vec<SharedString> = choices
            .iter()
            .map(|(id, name)| {
                if id.is_empty() {
                    direct_input.clone()
                } else {
                    name.as_str().into()
                }
            })
            .collect();
        let apps: Vec<AppRow> = snapshot
            .apps
            .iter()
            .map(|(app, id)| AppRow {
                app: app.as_str().into(),
                choice: choices
                    .iter()
                    .position(|(c, _)| c == id)
                    .and_then(|i| i32::try_from(i).ok())
                    .unwrap_or_default(),
            })
            .collect();
        backend.set_app_choices(model_of(names));
        backend.set_apps(model_of(apps));
        self.app_choices
            .replace(choices.into_iter().map(|(id, _)| id).collect());

        backend.set_daemon_version(snapshot.version.as_str().into());
        backend.set_checks(model_of(checks(&snapshot.diagnosis)));
    }

    fn show_catalog(&self, snapshot: &Snapshot) {
        let search = self.search.borrow();
        let entries: Vec<CatalogEntry> =
            model::catalog(&snapshot.input_methods, &search, &model::locale())
                .into_iter()
                .map(|entry| match entry {
                    model::CatalogEntry::Heading(group) => CatalogEntry {
                        heading: group.into(),
                        ..CatalogEntry::default()
                    },
                    model::CatalogEntry::Method(m) => CatalogEntry {
                        id: m.id.as_str().into(),
                        name: m.name.as_str().into(),
                        engine_name: m.engine_name.as_str().into(),
                        label: model::badge(m).into(),
                        heading: SharedString::default(),
                    },
                })
                .collect();
        self.backend(|b| {
            b.set_catalog_empty(entries.is_empty());
            b.set_catalog(model_of(entries));
        });
    }
}

/// An index from the window, which counts in `i32`.
fn index(i: i32) -> Option<usize> {
    usize::try_from(i).ok()
}

fn model_of<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn strings(items: Vec<String>) -> Vec<SharedString> {
    items.into_iter().map(SharedString::from).collect()
}

/// "200 ms", "1 s".
fn timeout_label(ms: u64) -> SharedString {
    if ms >= 1000 && ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000).into()
    } else {
        format!("{ms} ms").into()
    }
}

/// The lines of the status on the About page.
fn checks(diagnosis: &model::Diagnosis) -> Vec<Check> {
    let check = |kind: &str, subject: &str, ok: bool| Check {
        kind: kind.into(),
        subject: subject.into(),
        ok,
    };
    let mut checks = Vec::new();
    if !diagnosis.display.is_empty() {
        checks.push(check("display", &diagnosis.display, true));
    }
    for (seat, ours) in &diagnosis.seats {
        let kind = if *ours { "seat" } else { "seat-taken" };
        checks.push(check(kind, seat, *ours));
    }
    if diagnosis.focused_app {
        checks.push(check("focus", "", true));
    } else {
        checks.push(check("no-focus", "", false));
    }
    if !diagnosis.error.is_empty() {
        checks.push(check("error", &diagnosis.error, false));
    }
    checks
}

#[cfg(test)]
mod tests {
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use slint::Model as _;
    use slint::platform::{Key, WindowEvent};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::model::{Diagnosis, Engine, InputMethod, Options};
    use crate::window::View;

    fn method(id: &str, name: &str, label: &str, language: &str, enabled: bool) -> InputMethod {
        let engine = id.split('/').next().unwrap_or(id);
        InputMethod {
            id: id.to_owned(),
            name: name.to_owned(),
            engine: engine.to_owned(),
            engine_name: if engine == "fcitx5" { "Fcitx 5" } else { name }.to_owned(),
            label: label.to_owned(),
            language: language.to_owned(),
            enabled,
            in_use: id == "fcitx5/shuangpin",
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            version: "0.6.0".to_owned(),
            input_methods: vec![
                method("fcitx5/shuangpin", "双拼", "双", "zh_CN", true),
                method("fcitx5/pinyin", "拼音", "拼", "zh_CN", true),
                method("echo", "Echo", "", "en", true),
                method("fcitx5/mozc", "Mozc", "あ", "ja", false),
                method("fcitx5/cangjie", "倉頡", "倉", "zh_HK", false),
            ],
            engines: vec![
                Engine {
                    id: "fcitx5".to_owned(),
                    name: "Fcitx 5".to_owned(),
                    settings: true,
                },
                Engine {
                    id: "echo".to_owned(),
                    name: "Echo".to_owned(),
                    settings: false,
                },
            ],
            options: Options::default(),
            apps: vec![("firefox".to_owned(), "fcitx5/pinyin".to_owned())],
            diagnosis: Diagnosis {
                display: "wayland-1".to_owned(),
                seats: vec![("seat0".to_owned(), true)],
                focused_app: true,
                error: String::new(),
            },
        }
    }

    /// A window in English showing [`snapshot`], its controller, and what it asks
    /// omimed for.
    fn open() -> (SettingsWindow, Rc<Controller>, UnboundedReceiver<Command>) {
        i_slint_backend_testing::init_no_event_loop();
        let window = SettingsWindow::new().expect("a window");
        crate::tr::install(&window, "C");
        let (commands, requests) = unbounded_channel();
        let controller = Controller::new(&window, commands);
        controller.apply(Event::Snapshot(Box::new(snapshot())));
        window.show().expect("shown");
        (window, controller, requests)
    }

    /// The element labelled `label` that is not just text: the switch of a row, not its
    /// title.
    fn find(window: &SettingsWindow, label: &str) -> ElementHandle {
        let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(window, label)
            .filter(|e| e.accessible_role() != Some(AccessibleRole::Text))
            .collect();
        assert_eq!(found.len(), 1, "elements labelled {label:?}");
        found.into_iter().next().expect("one")
    }

    fn click(window: &SettingsWindow, label: &str) {
        find(window, label).invoke_accessible_default_action();
    }

    /// The `engines` that a request sets.
    fn engines(requests: &mut UnboundedReceiver<Command>) -> Vec<String> {
        match requests.try_recv() {
            Ok(Command::Configure(changes)) => match &changes[..] {
                [(key, Value::List(engines))] if key == "engines" => engines.clone(),
                other => panic!("not the engines: {other:?}"),
            },
            other => panic!("not a change of the configuration: {other:?}"),
        }
    }

    fn option(key: &str, value: Value) -> Command {
        Command::Configure(vec![(key.to_owned(), value)])
    }

    #[test]
    fn input_methods_move_and_go_with_their_buttons() {
        let (window, _controller, mut requests) = open();
        click(&window, "Move 拼音 up");
        assert_eq!(
            engines(&mut requests),
            ["fcitx5/pinyin", "fcitx5/shuangpin", "echo"]
        );
        click(&window, "Move 双拼 down");
        assert_eq!(
            engines(&mut requests),
            ["fcitx5/pinyin", "fcitx5/shuangpin", "echo"]
        );
        click(&window, "Remove Echo");
        assert_eq!(
            engines(&mut requests),
            ["fcitx5/shuangpin", "fcitx5/pinyin"]
        );
        // The first cannot go up, the last not down.
        click(&window, "Move 双拼 up");
        click(&window, "Move Echo down");
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn engines_with_settings_open_them() {
        let (window, _controller, mut requests) = open();
        click(&window, "Settings of 拼音");
        assert_eq!(
            requests.try_recv().ok(),
            Some(Command::OpenSettings("fcitx5".to_owned()))
        );
        // Echo has none: its button is not there to press.
        assert!(
            ElementHandle::find_by_accessible_label(&window, "Settings of Echo")
                .all(|e| e.accessible_enabled() == Some(false))
        );
    }

    #[test]
    fn the_dialog_adds_what_the_search_finds() {
        let (window, _controller, mut requests) = open();
        click(&window, "Add input method…");
        assert!(window.global::<View<'_>>().get_adding());
        let backend = window.global::<Backend<'_>>();
        let headings = |backend: &Backend<'_>| -> Vec<String> {
            backend
                .get_catalog()
                .iter()
                .map(|e| {
                    if e.id.is_empty() {
                        format!("# {}", e.heading)
                    } else {
                        e.name.to_string()
                    }
                })
                .collect()
        };
        assert_eq!(headings(&backend), ["# ja", "Mozc", "# zh-Hant", "倉頡"]);
        find(&window, "Search").set_accessible_value("moz");
        assert_eq!(headings(&backend), ["# ja", "Mozc"]);
        click(&window, "Add Mozc");
        assert_eq!(
            engines(&mut requests),
            ["fcitx5/shuangpin", "fcitx5/pinyin", "echo", "fcitx5/mozc"]
        );
        find(&window, "Search").set_accessible_value("nothing of the sort");
        assert!(backend.get_catalog_empty());
        click(&window, "Close");
        assert!(!window.global::<View<'_>>().get_adding());
    }

    #[test]
    fn pressed_keys_become_the_hotkey() {
        let (window, _controller, mut requests) = open();
        let view = window.global::<View<'_>>();
        view.set_page(1);
        let backend = window.global::<Backend<'_>>();
        let keys = |keys: ModelRc<SharedString>| -> Vec<String> {
            keys.iter().map(|k| k.to_string()).collect()
        };
        assert_eq!(keys(backend.get_next_keys()), ["Ctrl", "Shift", "Space"]);

        click(&window, "Change Next input method");
        assert_eq!(view.get_recording(), "next");
        let press = |text: SharedString| {
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        };
        window.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Meta.into(),
        });
        assert_eq!(view.get_recording(), "next", "a modifier alone");
        press("k".into());
        window.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Meta.into(),
        });
        assert_eq!(view.get_recording(), "");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("hotkeys.next", Value::String("Super+k".to_owned())))
        );

        click(&window, "Change Direct input and back");
        press(Key::Escape.into());
        assert_eq!(view.get_recording(), "");
        assert!(requests.try_recv().is_err(), "Escape cancels");

        click(&window, "Turn Direct input and back off");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("hotkeys.toggle", Value::String(String::new())))
        );
    }

    #[test]
    fn switches_and_choices_set_their_options() {
        let (window, _controller, mut requests) = open();
        let view = window.global::<View<'_>>();
        let backend = window.global::<Backend<'_>>();

        view.set_page(2);
        click(&window, "Remember the input method of each application");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("apps.remember", Value::Bool(true)))
        );
        // Shown once omimed has it.
        let mut remembering = snapshot();
        remembering.options.remember_apps = true;
        _controller.apply(Event::Snapshot(Box::new(remembering)));
        click(&window, "Forget firefox");
        assert_eq!(
            requests.try_recv().ok(),
            Some(Command::ForgetApp("firefox".to_owned()))
        );
        let choices: Vec<String> = backend
            .get_app_choices()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(choices, ["Direct input", "双拼", "拼音", "Echo"]);
        assert_eq!(backend.get_apps().row_data(0).map(|a| a.choice), Some(2));
        backend.invoke_set_app_input_method("firefox".into(), 0);
        assert_eq!(
            requests.try_recv().ok(),
            Some(Command::SetAppInputMethod(
                "firefox".to_owned(),
                String::new()
            ))
        );

        view.set_page(3);
        click(&window, "Show in the tray");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("tray.enabled", Value::Bool(false)))
        );
        click(&window, "Show the switcher");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("panel.enabled", Value::Bool(false)))
        );
        click(&window, "Show notifications");
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("notifications.enabled", Value::Bool(false)))
        );
        assert_eq!(backend.get_timeout_index(), 1, "200 ms");
        backend.invoke_set_timeout(3);
        assert_eq!(
            requests.try_recv().ok(),
            Some(option("engine.key_timeout_ms", Value::Integer(500)))
        );
    }

    #[test]
    fn the_window_says_when_omimed_is_away_or_something_failed() {
        let (window, controller, _requests) = open();
        let backend = window.global::<Backend<'_>>();
        assert!(backend.get_connected());
        controller.apply(Event::Failed("`echo` has no settings".to_owned()));
        assert_eq!(backend.get_error(), "`echo` has no settings");
        click(&window, "Dismiss");
        assert_eq!(backend.get_error(), "");
        controller.apply(Event::Absent);
        assert!(!backend.get_connected());
        assert!(
            ElementHandle::find_by_accessible_label(&window, "Remove Echo")
                .next()
                .is_none(),
            "no settings without omimed"
        );
        view_about(&window);
    }

    /// The About page lists what omimed reported.
    fn view_about(window: &SettingsWindow) {
        let backend = window.global::<Backend<'_>>();
        let kinds: Vec<String> = backend
            .get_checks()
            .iter()
            .map(|c| c.kind.to_string())
            .collect();
        assert_eq!(kinds, ["display", "seat", "focus"]);
        assert_eq!(backend.get_daemon_version(), "0.6.0");
    }
}
