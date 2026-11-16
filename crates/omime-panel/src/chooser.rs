//! The chooser, `omime choose`: the input methods in a list, to browse with the
//! keyboard or the pointer and choose one. Unlike the switcher, a notice of a switch
//! that happened, it takes the keyboard while it is open, and switches only when the
//! user confirms; the choice is for the application the user was in.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::StreamExt as _;
use omime_proto::tr;
use slint::{ComponentHandle as _, ModelRc, VecModel};
use tokio::sync::mpsc;
use zbus::OwnedVariant;

use super::{
    DaemonProxy, InputMethodsChangedStream, SettingsProxy, StatusChangedStream, read_dark,
};

/// One chooser at a time: who has this name on the session bus is it.
const BUS_NAME: &str = "io.github.ign1x.OhMyIme.Chooser";
/// How many input methods the list shows at once; it scrolls through more.
const PAGE_SIZE: usize = 6;
/// How wide the chooser is, in logical pixels.
const WIDTH: u32 = 420;
/// How long omimed has to confirm a choice.
const CONFIRMATION: Duration = Duration::from_secs(5);

slint::slint! {
    export { Chooser, Look, Row } from "../ui/chooser.slint";
}

/// An input method to choose, or direct input (an empty `id`).
#[derive(Debug, Clone)]
struct Choice {
    id: String,
    label: String,
    name: String,
    /// The name of its engine, when it is not the input method's own.
    detail: String,
    current: bool,
}

/// What the chooser offers: direct input, then the enabled input methods in switching
/// order (`InputMethods` of the control API), with `current` (empty: direct input) in
/// use.
fn choices(methods: &[HashMap<String, OwnedVariant>], current: &str) -> Vec<Choice> {
    let text = |method: &HashMap<String, OwnedVariant>, key: &str| {
        method
            .get(key)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    let enabled = |method: &&HashMap<String, OwnedVariant>| {
        method
            .get("enabled")
            .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
            .unwrap_or(false)
    };
    let mut result = vec![Choice {
        id: String::new(),
        label: "A".into(),
        name: tr!("Direct input"),
        detail: String::new(),
        current: current.is_empty(),
    }];
    for method in methods.iter().filter(enabled) {
        let id = text(method, "id");
        let name = text(method, "name");
        let mut label = text(method, "label");
        if label.is_empty() {
            label = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
        }
        let engine = text(method, "engine-name");
        result.push(Choice {
            current: id == current,
            detail: if engine == name {
                String::new()
            } else {
                engine
            },
            id,
            label,
            name,
        });
    }
    result
}

/// Where the user is in the list, and the part of it that shows.
#[derive(Debug)]
struct Selection {
    choices: Vec<Choice>,
    selected: usize,
    /// The first choice that shows.
    start: usize,
}

impl Selection {
    /// Starts at the input method in use.
    fn new(choices: Vec<Choice>) -> Self {
        let selected = choices.iter().position(|c| c.current).unwrap_or(0);
        let mut this = Self {
            choices,
            selected,
            start: 0,
        };
        this.reveal();
        this
    }

    /// Scrolls so that the selected choice shows.
    fn reveal(&mut self) {
        if self.selected < self.start {
            self.start = self.selected;
        }
        if self.selected >= self.start + PAGE_SIZE {
            self.start = self.selected + 1 - PAGE_SIZE;
        }
    }

    /// Moves by `delta` choices, around the ends of the list.
    fn step(&mut self, delta: i32) {
        self.selected = (self.selected as i64 + i64::from(delta))
            .rem_euclid(self.choices.len() as i64) as usize;
        self.reveal();
    }

    /// Moves to the first choice, or the last.
    fn edge(&mut self, last: bool) {
        self.selected = if last { self.choices.len() - 1 } else { 0 };
        self.reveal();
    }

    /// Takes a changed list, staying on the selected input method while it is there.
    fn update(&mut self, choices: Vec<Choice>) {
        let id = self.choices[self.selected].id.clone();
        self.selected = choices.iter().position(|c| c.id == id).unwrap_or(0);
        self.choices = choices;
        self.start = self.start.min(self.selected);
        self.reveal();
    }
}

/// The chooser on the screen, on the thread of the user interface.
struct View {
    ui: Chooser,
    overlay: omime_overlay::Overlay,
    state: RefCell<Selection>,
    /// The id of a confirmed choice, for the thread that talks to omimed.
    chosen: mpsc::UnboundedSender<String>,
}

impl View {
    /// Shows the list as it is now, in a chooser as high as it needs.
    fn refresh(&self) {
        let state = self.state.borrow();
        let rows: Vec<Row> = state
            .choices
            .iter()
            .enumerate()
            .skip(state.start)
            .take(PAGE_SIZE)
            .map(|(i, choice)| Row {
                label: choice.label.clone().into(),
                name: choice.name.clone().into(),
                detail: choice.detail.clone().into(),
                current: choice.current,
                selected: i == state.selected,
            })
            .collect();
        self.ui.set_rows(ModelRc::new(VecModel::from(rows)));
        self.ui
            .set_counter(format!("{} / {}", state.selected + 1, state.choices.len()).into());
        tracing::debug!(count = state.choices.len(), "chooser catalog updated");
        let height = self.ui.get_wanted_height().ceil().max(1.0) as u32;
        self.overlay.show(WIDTH, height);
        self.ui.invoke_focus_keys();
    }

    /// Changes where the user is in the list.
    fn browse(&self, change: impl FnOnce(&mut Selection)) {
        change(&mut self.state.borrow_mut());
        self.refresh();
    }

    /// Confirms the choice at `index` of the list, or the selected one.
    fn choose(&self, index: Option<usize>) {
        if self.ui.get_busy() {
            return;
        }
        let state = self.state.borrow();
        let Some(choice) = state.choices.get(index.unwrap_or(state.selected)) else {
            return;
        };
        self.ui.set_busy(true);
        let _ = self.chosen.send(choice.id.clone());
    }

    fn close(&self) {
        self.overlay.hide();
        let _ = slint::quit_event_loop();
    }
}

thread_local! {
    static VIEW: RefCell<Option<Rc<View>>> = const { RefCell::new(None) };
}

/// Does `f` with the chooser, on the thread of the user interface.
fn with_view(f: impl FnOnce(&View) + Send + 'static) -> Result<(), slint::EventLoopError> {
    slint::invoke_from_event_loop(move || {
        VIEW.with(|slot| {
            if let Some(view) = slot.borrow().as_ref() {
                f(view);
            }
        });
    })
}

/// What the chooser starts with, and follows omimed by.
struct Session {
    conn: zbus::Connection,
    /// The application the user was in: the choice is for it.
    app: String,
    /// The input method in use.
    current: String,
    methods: Vec<HashMap<String, OwnedVariant>>,
    dark: bool,
    changes: InputMethodsChangedStream,
    statuses: StatusChangedStream,
    /// omimed leaving or coming to the session bus.
    owners: zbus::fdo::NameOwnerChangedStream,
}

/// Becomes the chooser of the session and asks omimed what there is to choose; `None`
/// when a chooser is open already.
async fn prepare() -> anyhow::Result<Option<Session>> {
    let conn = zbus::Connection::session().await?;
    match conn
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DO_NOT_QUEUE)
        .await
    {
        Ok(_) => {}
        Err(zbus::Error::NameTaken) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let daemon = DaemonProxy::new(&conn).await?;
    // Subscribe before reading the snapshot so a catalog update or daemon
    // restart between the query and mapping cannot be missed.
    let changes = daemon.receive_input_methods_changed().await?;
    let statuses = daemon.receive_status_changed().await?;
    let owners = zbus::fdo::DBusProxy::new(&conn)
        .await?
        .receive_name_owner_changed_with_args(&[(0, omime_proto::names::DAEMON_BUS_NAME)])
        .await?;
    let (app, current) = daemon.choice_context().await.context(tr!(
        "cannot open the input method chooser; is omimed running?"
    ))?;
    let methods = daemon.input_methods().await?;
    let dark = match SettingsProxy::new(&conn).await {
        Ok(settings) => read_dark(&settings).await,
        Err(_) => true,
    };
    Ok(Some(Session {
        conn,
        app,
        current,
        methods,
        dark,
        changes,
        statuses,
        owners,
    }))
}

/// Sends confirmed choices to omimed and keeps the list as omimed has it, until the
/// choice is made or omimed goes away.
async fn follow(session: Session, mut chosen: mpsc::UnboundedReceiver<String>) {
    let Session {
        conn,
        app,
        mut current,
        mut changes,
        mut statuses,
        mut owners,
        ..
    } = session;
    let Ok(daemon) = DaemonProxy::new(&conn).await else {
        return;
    };
    loop {
        tokio::select! {
            choice = chosen.recv() => {
                let Some(choice) = choice else { break };
                let confirmed = tokio::time::timeout(
                    CONFIRMATION,
                    daemon.choose_input_method(&choice, &app),
                )
                .await;
                let error = match confirmed {
                    Ok(Ok(())) => {
                        let _ = with_view(View::close);
                        break;
                    }
                    Ok(Err(e)) => e.to_string(),
                    Err(_) => tr!("the daemon did not confirm the selection"),
                };
                let _ = with_view(move |view| {
                    view.ui.set_busy(false);
                    view.ui.set_error(error.into());
                    view.refresh();
                });
            }
            change = changes.next() => {
                if change.is_none() {
                    break;
                }
                let Ok(methods) = daemon.input_methods().await else { break };
                let current = current.clone();
                let _ = with_view(move |view| {
                    view.browse(|state| state.update(choices(&methods, &current)));
                });
            }
            status = statuses.next() => {
                let Some(status) = status else { break };
                let Ok(args) = status.args() else { continue };
                current = args
                    .status()
                    .get("input-method")
                    .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
                    .unwrap_or_default();
                let current = current.clone();
                let _ = with_view(move |view| {
                    view.browse(|state| {
                        for choice in &mut state.choices {
                            choice.current = choice.id == current;
                        }
                    });
                });
            }
            _ = owners.next() => break,
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let Some(session) = runtime.block_on(prepare())? else {
        return Ok(());
    };
    let overlay = omime_overlay::install_modal(
        "omime-chooser",
        omime_overlay::Placement {
            above_fullscreen: true,
            ..Default::default()
        },
    )?;
    let ui = Chooser::new()?;
    ui.global::<Look<'_>>().set_dark(session.dark);
    ui.set_heading(tr!("Choose input method").into());
    ui.set_browse(tr!("Browse").into());
    ui.set_select(tr!("Select").into());
    ui.set_dismiss(tr!("Cancel").into());
    let (chosen, choices_made) = mpsc::unbounded_channel::<String>();
    let view = Rc::new(View {
        ui,
        overlay,
        state: RefCell::new(Selection::new(choices(&session.methods, &session.current))),
        chosen,
    });
    let weak = Rc::downgrade(&view);
    view.ui.on_navigate(move |delta| {
        if let Some(view) = weak.upgrade() {
            view.browse(|state| state.step(delta));
        }
    });
    let weak = Rc::downgrade(&view);
    view.ui.on_edge(move |last| {
        if let Some(view) = weak.upgrade() {
            view.browse(|state| state.edge(last));
        }
    });
    let weak = Rc::downgrade(&view);
    view.ui.on_confirm(move || {
        if let Some(view) = weak.upgrade() {
            view.choose(None);
        }
    });
    let weak = Rc::downgrade(&view);
    view.ui.on_pick(move |index| {
        if let Some(view) = weak.upgrade() {
            let start = view.state.borrow().start;
            view.choose(Some(start + index as usize));
        }
    });
    let weak = Rc::downgrade(&view);
    view.ui.on_cancel(move || {
        if let Some(view) = weak.upgrade() {
            view.close();
        }
    });
    view.ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    view.ui.show()?;
    view.refresh();
    VIEW.with(|slot| slot.borrow_mut().replace(view));
    std::thread::Builder::new()
        .name("omime-chooser-bus".into())
        .spawn(move || {
            runtime.block_on(follow(session, choices_made));
            // Chosen, or omimed went away: there is nothing left to choose.
            let _ = slint::quit_event_loop();
        })?;
    tracing::info!("interactive chooser opened");
    slint::run_event_loop()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsing_large_lists_reveals_selection_and_keeps_ids_across_updates() {
        let list = (0..20)
            .map(|i| Choice {
                id: i.to_string(),
                label: String::new(),
                name: i.to_string(),
                detail: String::new(),
                current: i == 12,
            })
            .collect();
        let mut s = Selection::new(list);
        assert_eq!((s.selected, s.start), (12, 7));
        s.step(6);
        assert_eq!((s.selected, s.start), (18, 13));
        s.step(2);
        assert_eq!((s.selected, s.start), (0, 0));
        s.step(-1);
        assert_eq!((s.selected, s.start), (19, 14));
        s.edge(false);
        assert_eq!((s.selected, s.start), (0, 0));
        s.edge(true);
        assert_eq!((s.selected, s.start), (19, 14));
        let mut list = s.choices.clone();
        list.remove(0);
        s.update(list);
        assert_eq!(s.choices[s.selected].id, "19");
        s.update(vec![Choice {
            id: String::new(),
            label: "A".into(),
            name: "Direct".into(),
            detail: String::new(),
            current: true,
        }]);
        assert_eq!((s.selected, s.start), (0, 0));
    }

    #[test]
    fn an_engine_is_named_beside_its_input_methods_only() {
        let method = |id: &str, name: &str, engine: &str| {
            let owned =
                |v: zbus::Variant<'_>| OwnedVariant::try_from(v).expect("no file descriptors");
            HashMap::from([
                ("id".to_owned(), owned(zbus::Variant::from(id))),
                ("name".to_owned(), owned(zbus::Variant::from(name))),
                ("engine-name".to_owned(), owned(zbus::Variant::from(engine))),
                ("enabled".to_owned(), owned(zbus::Variant::from(true))),
            ])
        };
        let listed = choices(
            &[
                method("rime/luna_pinyin", "朙月拼音", "Rime"),
                method("sparkle", "Sparkle", "Sparkle"),
            ],
            "sparkle",
        );
        let shown: Vec<(&str, &str, &str, bool)> = listed
            .iter()
            .map(|c| {
                (
                    c.label.as_str(),
                    c.name.as_str(),
                    c.detail.as_str(),
                    c.current,
                )
            })
            .collect();
        assert_eq!(
            shown[1..],
            [
                ("朙", "朙月拼音", "Rime", false),
                ("S", "Sparkle", "", true)
            ]
        );
        assert_eq!((shown[0].0, shown[0].3), ("A", false));
    }
}
