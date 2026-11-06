//! Explicit selection, separate from the passive switch notification.
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

use super::{DaemonProxy, SettingsProxy, read_dark};

const BUS_NAME: &str = "io.github.ign1x.OhMyIme.Chooser";
const PAGE_SIZE: usize = 6;

slint::slint! {
    export struct Row { label: string, name: string, selected: bool, current: bool }
    export component Chooser inherits Window {
        title: "oh-my-ime";
        background: transparent;
        in property <[Row]> rows;
        in property <bool> dark: true;
        in property <bool> busy;
        in property <string> heading;
        in property <string> hint;
        in property <string> counter;
        in property <string> error;
        callback navigate(int);
        callback edge(bool);
        callback pick(int);
        callback confirm();
        callback cancel();
        public function focus-keys() { keys.focus(); }
        Rectangle {
            background: root.dark ? #202124 : #f8f8fa;
            border-radius: 16px;
            border-width: 1px;
            border-color: root.dark ? #55565a : #ccccd0;
            VerticalLayout {
                padding: 12px;
                spacing: 4px;
                HorizontalLayout {
                    height: 32px;
                    Text { text: root.heading; font-weight: 600; color: root.dark ? #f1f1f3 : #1d1d1f; }
                    Text { text: root.counter; horizontal-alignment: right; color: root.dark ? #b9b9be : #6c6c71; }
                }
                for row[index] in root.rows: Rectangle {
                    height: 48px;
                    border-radius: 9px;
                    background: row.selected ? #3478f6 : transparent;
                    accessible-role: button;
                    accessible-label: row.name;
                    accessible-action-default => { root.pick(index); }
                    HorizontalLayout {
                        padding: 8px;
                        spacing: 12px;
                        Text { width: 40px; text: row.label; font-size: 22px; color: row.selected || root.dark ? #ffffff : #1d1d1f; }
                        Text { text: row.name; overflow: elide; color: row.selected || root.dark ? #ffffff : #1d1d1f; }
                        Text { width: 18px; text: row.current ? "✓" : ""; color: row.selected || root.dark ? #ffffff : #1d1d1f; }
                    }
                    TouchArea {
                        enabled: !root.busy;
                        clicked => { root.pick(index); }
                        scroll-event(event) => {
                            if event.delta-y != 0px { root.navigate(event.delta-y < 0px ? 1 : -1); }
                            accept
                        }
                    }
                }
                if root.error != "": Text {
                    text: root.error; wrap: word-wrap; color: root.dark ? #ffb4ab : #b3261e;
                    height: 54px;
                }
                Text { height: 28px; text: root.hint; font-size: 11px; color: root.dark ? #b9b9be : #6c6c71; }
            }
        }
        keys := FocusScope {
            key-pressed(event) => {
                if root.busy { return accept; }
                if event.text == Key.Escape { root.cancel(); }
                else if event.text == Key.Return || event.text == " " { root.confirm(); }
                else if event.text == Key.UpArrow || event.text == Key.LeftArrow { root.navigate(-1); }
                else if event.text == Key.DownArrow || event.text == Key.RightArrow { root.navigate(1); }
                else if event.text == Key.Tab { root.navigate(event.modifiers.shift ? -1 : 1); }
                else if event.text == Key.PageUp { root.navigate(-6); }
                else if event.text == Key.PageDown { root.navigate(6); }
                else if event.text == Key.Home { root.edge(false); }
                else if event.text == Key.End { root.edge(true); }
                accept
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Choice {
    id: String,
    label: String,
    name: String,
    current: bool,
}

fn choices(methods: &[HashMap<String, OwnedVariant>], current: &str) -> Vec<Choice> {
    let text = |m: &HashMap<String, OwnedVariant>, key: &str| {
        m.get(key)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    let mut result = vec![Choice {
        id: String::new(),
        label: "A".into(),
        name: tr!("Direct input"),
        current: current.is_empty(),
    }];
    for method in methods {
        if !method
            .get("enabled")
            .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
            .unwrap_or(false)
        {
            continue;
        }
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
        result.push(Choice {
            current: id == current,
            id,
            label,
            name,
        });
    }
    result
}

#[derive(Debug)]
struct Selection {
    choices: Vec<Choice>,
    selected: usize,
    start: usize,
}
impl Selection {
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
    fn reveal(&mut self) {
        if self.selected < self.start {
            self.start = self.selected;
        }
        if self.selected >= self.start + PAGE_SIZE {
            self.start = self.selected + 1 - PAGE_SIZE;
        }
    }
    fn step(&mut self, delta: i32) {
        self.selected = (self.selected as i64 + i64::from(delta))
            .rem_euclid(self.choices.len() as i64) as usize;
        self.reveal();
    }
    fn update(&mut self, choices: Vec<Choice>) {
        let id = self.choices[self.selected].id.clone();
        self.selected = choices.iter().position(|c| c.id == id).unwrap_or(0);
        self.choices = choices;
        self.start = self.start.min(self.selected);
        self.reveal();
    }
}

struct View {
    ui: Chooser,
    overlay: omime_overlay::Overlay,
    state: RefCell<Selection>,
    commands: mpsc::UnboundedSender<String>,
}
impl View {
    fn refresh(&self) {
        let state = self.state.borrow();
        let rows = state
            .choices
            .iter()
            .enumerate()
            .skip(state.start)
            .take(PAGE_SIZE)
            .map(|(i, c)| Row {
                label: c.label.clone().into(),
                name: c.name.clone().into(),
                current: c.current,
                selected: i == state.selected,
            })
            .collect::<Vec<_>>();
        self.ui.set_rows(ModelRc::new(VecModel::from(rows)));
        self.ui
            .set_counter(format!("{} / {}", state.selected + 1, state.choices.len()).into());
        tracing::debug!(count = state.choices.len(), "chooser catalog updated");
        let height = 24
            + 32
            + 28
            + state.choices.len().min(PAGE_SIZE) * 52
            + if self.ui.get_error().is_empty() {
                8
            } else {
                66
            };
        self.overlay.show(440, height as u32);
        self.ui.invoke_focus_keys();
    }
    fn choose(&self, index: Option<usize>) {
        if self.ui.get_busy() {
            return;
        }
        let state = self.state.borrow();
        let Some(choice) = state.choices.get(index.unwrap_or(state.selected)) else {
            return;
        };
        self.ui.set_busy(true);
        let _ = self.commands.send(choice.id.clone());
    }
    fn close(&self) {
        self.overlay.hide();
        let _ = slint::quit_event_loop();
    }
}

thread_local! { static VIEW: RefCell<Option<Rc<View>>> = const { RefCell::new(None) }; }
fn view(f: impl FnOnce(&View) + Send + 'static) -> Result<(), slint::EventLoopError> {
    slint::invoke_from_event_loop(move || {
        VIEW.with(|slot| {
            if let Some(v) = slot.borrow().as_ref() {
                f(v);
            }
        })
    })
}

pub fn run() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let prepared = runtime.block_on(async {
        let conn = zbus::Connection::session().await?;
        match conn
            .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DO_NOT_QUEUE)
            .await
        {
            Ok(_) => {}
            Err(zbus::Error::NameTaken) => return Ok::<_, anyhow::Error>(None),
            Err(e) => return Err(e.into()),
        }
        let daemon = DaemonProxy::new(&conn).await?;
        // Subscribe before reading the snapshot so a catalog update or daemon
        // restart between the query and mapping cannot be missed.
        let changes = daemon.receive_input_methods_changed().await?;
        let statuses = daemon.receive_status_changed().await?;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let owner_changes = dbus
            .receive_name_owner_changed_with_args(&[(0, omime_proto::names::DAEMON_BUS_NAME)])
            .await?;
        let (app, current) = daemon.choice_context().await.context(tr!(
            "cannot open the input method chooser; is omimed running?"
        ))?;
        let methods = daemon.input_methods().await?;
        let dark = match SettingsProxy::new(&conn).await {
            Ok(p) => read_dark(&p).await,
            Err(_) => true,
        };
        Ok(Some((
            conn,
            app,
            current,
            methods,
            dark,
            changes,
            statuses,
            owner_changes,
        )))
    })?;
    let Some((conn, app, mut current, methods, dark, mut changes, mut statuses, mut owners)) =
        prepared
    else {
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
    let (commands, mut selected) = mpsc::unbounded_channel::<String>();
    let v = Rc::new(View {
        ui,
        overlay,
        state: RefCell::new(Selection::new(choices(&methods, &current))),
        commands,
    });
    v.ui.set_heading(tr!("Choose input method").into());
    v.ui.set_hint(tr!("↑ ↓  Browse    Enter  Select    Esc  Cancel").into());
    v.ui.set_dark(dark);
    let weak = Rc::downgrade(&v);
    v.ui.on_navigate(move |delta| {
        if let Some(v) = weak.upgrade() {
            v.state.borrow_mut().step(delta);
            v.refresh();
        }
    });
    let weak = Rc::downgrade(&v);
    v.ui.on_edge(move |last| {
        if let Some(v) = weak.upgrade() {
            {
                let mut s = v.state.borrow_mut();
                s.selected = if last { s.choices.len() - 1 } else { 0 };
                s.reveal();
            }
            v.refresh();
        }
    });
    let weak = Rc::downgrade(&v);
    v.ui.on_confirm(move || {
        if let Some(v) = weak.upgrade() {
            v.choose(None);
        }
    });
    let weak = Rc::downgrade(&v);
    v.ui.on_pick(move |index| {
        if let Some(v) = weak.upgrade() {
            let start = v.state.borrow().start;
            v.choose(Some(start + index as usize));
        }
    });
    let weak = Rc::downgrade(&v);
    v.ui.on_cancel(move || {
        if let Some(v) = weak.upgrade() {
            v.close();
        }
    });
    v.ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    v.ui.show()?;
    v.refresh();
    VIEW.with(|slot| slot.borrow_mut().replace(v));
    std::thread::Builder::new().name("omime-chooser-bus".into()).spawn(move || runtime.block_on(async move {
        let daemon = match DaemonProxy::new(&conn).await { Ok(p) => p, Err(_) => { let _=slint::quit_event_loop(); return; } };
        loop {
            tokio::select! {
                choice = selected.recv() => {
                    let Some(choice)=choice else {break;};
                    let result = tokio::time::timeout(Duration::from_secs(5), daemon.choose_input_method(&choice,&app)).await;
                    match result {
                        Ok(Ok(())) => { let _=view(|v| v.close()); break; }
                        error => {
                            let error=match error {Ok(Err(e)) => e.to_string(), _ => tr!("the daemon did not confirm the selection")};
                            let _=view(move |v| {v.ui.set_busy(false); v.ui.set_error(error.into()); v.refresh();});
                        }
                    }
                }
                change=changes.next() => {
                    if change.is_none() {break;}
                    match daemon.input_methods().await {
                        Ok(methods) => { let current=current.clone(); let _=view(move |v| {v.state.borrow_mut().update(choices(&methods,&current)); v.refresh();}); }
                        Err(_) => break,
                    }
                }
                status = statuses.next() => {
                    let Some(status) = status else { break; };
                    if let Ok(args) = status.args() {
                        current = args.status().get("input-method")
                            .and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
                        let current = current.clone();
                        let _ = view(move |v| {
                            for c in &mut v.state.borrow_mut().choices { c.current = c.id == current; }
                            v.refresh();
                        });
                    }
                }
                _ = owners.next() => break,
            }
        }
        let _=slint::quit_event_loop();
    }))?;
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
        let mut list = s.choices.clone();
        list.remove(0);
        s.update(list);
        assert_eq!(s.choices[s.selected].id, "19");
        s.update(vec![Choice {
            id: String::new(),
            label: "A".into(),
            name: "Direct".into(),
            current: true,
        }]);
        assert_eq!((s.selected, s.start), (0, 0));
    }
}
