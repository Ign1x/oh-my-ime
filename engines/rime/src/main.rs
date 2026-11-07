//! `omime-rime`: oh-my-ime's Rime engine. Rime (librime) does the input: schemas such as
//! Xiaohe shuangpin (`double_pinyin_flypy`, the default) and pinyin, which users choose
//! as input methods of their own (`rime/double_pinyin_flypy`). The engine draws the
//! candidates in its own Slint window, a popup at the text cursor; with an application
//! that leaves the preedit to the input method (xterm), the window shows it too.
//!
//! Each text field has a Rime session of its own; Chinese or English letters (Rime's
//! `ascii_mode`) are the same in all of them. The engine's menu switches them, full or
//! half width, simplified or traditional characters and punctuation, and deploys again
//! after the user changed Rime's configuration. While Rime deploys, which it does the
//! first time and after changes in its data, keys go to applications as they are.
//!
//! `omime-rime --settings` is its settings window (see [`settings`]).
//!
//! The engine logic runs on the SDK's thread and the window owns the main thread; both
//! use librime under one lock.

mod data;
mod names;
mod rime;
mod settings;
mod style;

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use omime_sdk::{
    Context, ContextId, Engine, Host, InputMethodInfo, KeyEvent, Preedit, Property, PropertyKind,
    Status,
};
use rime::{Composing, Notice, Rime, SessionId};
use slint::{ComponentHandle as _, ModelRc, VecModel};

slint::slint! {
    import { Palette } from "std-widgets.slint";

    export struct Item {
        label: string,
        text: string,
        comment: string,
    }

    // How the window looks, after the engine's settings (rime.toml).
    export global Look {
        // 0 as the desktop prefers, 1 light, 2 dark.
        in-out property <int> appearance;
        // Candidates in a column rather than a row.
        in-out property <bool> vertical;
        // Of the candidates; labels, comments and the preedit are in proportion.
        in-out property <length> font-size: 18px;
        out property <bool> dark: self.appearance == 2 || (self.appearance == 0 && Palette.color-scheme == ColorScheme.dark);
        out property <color> ink: self.dark ? #f2f2f7 : #1c1c1e;
        out property <color> faint: self.dark ? #ffffff8c : #0000008c;
        out property <color> accent: self.dark ? #0a84ff : #007aff;
        out property <color> hover: self.dark ? #ffffff14 : #0000000d;
    }

    // A candidate, which a click picks.
    component Cell inherits Rectangle {
        in property <Item> item;
        in property <bool> highlighted;
        callback clicked();

        border-radius: 7px;
        background: root.highlighted ? Look.accent : touch.has-hover ? Look.hover : transparent;
        HorizontalLayout {
            // In a column, the candidate stays at the start of its row.
            alignment: start;
            padding-left: 7px;
            padding-right: 7px;
            padding-top: 3px;
            padding-bottom: 3px;
            spacing: 4px;
            Text {
                text: root.item.label;
                font-size: Look.font-size * 2 / 3;
                vertical-alignment: center;
                color: root.highlighted ? #ffffffcc : Look.faint;
            }
            Text {
                text: root.item.text;
                font-size: Look.font-size;
                vertical-alignment: center;
                color: root.highlighted ? #ffffff : Look.ink;
            }
            if root.item.comment != "": Text {
                text: root.item.comment;
                font-size: Look.font-size * 2 / 3;
                vertical-alignment: center;
                color: root.highlighted ? #ffffffcc : Look.faint;
            }
        }
        touch := TouchArea {
            mouse-cursor: pointer;
            clicked => { root.clicked(); }
        }
    }

    // An arrow that turns the page.
    component Arrow inherits Rectangle {
        in property <string> glyph;
        in property <bool> active;
        callback clicked();

        width: Look.font-size;
        Text {
            text: root.glyph;
            font-size: Look.font-size;
            horizontal-alignment: center;
            vertical-alignment: center;
            color: root.active ? Look.ink : Look.faint;
        }
        TouchArea {
            enabled: root.active;
            clicked => { root.clicked(); }
        }
    }

    export component Candidates inherits Window {
        title: "Rime";
        no-frame: true;
        background: transparent;

        // The preedit, when the application does not show it.
        in property <string> preedit;
        in property <[Item]> items;
        in property <int> highlighted;
        in property <bool> has-previous;
        in property <bool> has-next;
        callback picked(int);
        // `true` turns back a page.
        callback turn(bool);

        // How large the window has to be, for the engine to resize it.
        out property <length> wanted-width: panel.preferred-width + 8px;
        out property <length> wanted-height: panel.preferred-height + 8px;

        panel := Rectangle {
            x: 4px;
            y: 4px;
            width: self.preferred-width;
            height: self.preferred-height;
            border-radius: 10px;
            background: Look.dark ? #2c2c2ef2 : #fbfbfdf5;
            border-width: 1px;
            border-color: Look.dark ? #ffffff1f : #0000001f;

            // The wheel turns pages. Under the candidates, which take the clicks.
            TouchArea {
                scroll-event(event) => {
                    if (event.delta-y > 0) {
                        root.turn(true);
                        return accept;
                    }
                    if (event.delta-y < 0) {
                        root.turn(false);
                        return accept;
                    }
                    reject
                }
            }

            VerticalLayout {
                padding: 6px;
                spacing: 4px;

                if root.preedit != "": Text {
                    text: root.preedit;
                    color: Look.ink;
                    font-size: Look.font-size * 5 / 6;
                }

                if !Look.vertical: HorizontalLayout {
                    spacing: 2px;
                    for item[i] in root.items: Cell {
                        item: item;
                        highlighted: i == root.highlighted;
                        clicked => { root.picked(i); }
                    }
                    if root.has-previous || root.has-next: HorizontalLayout {
                        padding-left: 4px;
                        spacing: 2px;
                        Arrow {
                            glyph: "‹";
                            active: root.has-previous;
                            clicked => { root.turn(true); }
                        }
                        Arrow {
                            glyph: "›";
                            active: root.has-next;
                            clicked => { root.turn(false); }
                        }
                    }
                }

                if Look.vertical: VerticalLayout {
                    spacing: 2px;
                    for item[i] in root.items: Cell {
                        item: item;
                        highlighted: i == root.highlighted;
                        clicked => { root.picked(i); }
                    }
                    if root.has-previous || root.has-next: HorizontalLayout {
                        alignment: end;
                        spacing: 2px;
                        Arrow {
                            glyph: "‹";
                            active: root.has-previous;
                            clicked => { root.turn(true); }
                        }
                        Arrow {
                            glyph: "›";
                            active: root.has-next;
                            clicked => { root.turn(false); }
                        }
                    }
                }
            }
        }
    }
}

/// What a text field is to the engine.
#[derive(Debug, Clone, Default)]
struct Field {
    session: Option<SessionId>,
    /// Whether the application shows the preedit itself.
    client_preedit: bool,
}

/// The engine's state, shared by its thread, the window's and librime's notices.
#[derive(Default)]
struct State {
    /// None when librime could not start.
    rime: Option<Rime>,
    deployed: bool,
    host: Option<Host>,
    fields: BTreeMap<ContextId, Field>,
    /// The field the window is about.
    focused: Option<ContextId>,
    /// The schema in use, for new sessions.
    schema: Option<String>,
    /// English letters (Rime's ascii_mode), the same in every field.
    ascii: bool,
    /// What the menu last showed, to announce it again only when it changed.
    menu: Vec<Property>,
    /// Whether the platform was asked to show the window.
    shown: bool,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a field needs after Rime handled something: text to commit, and its preedit.
#[derive(Debug, Default)]
struct Effects {
    commit: Option<String>,
    preedit: Option<Preedit>,
}

/// A schema's badge: 双 for the double pinyin schemas, 拼 for the others of pinyin,
/// else the first character of its name.
fn badge(id: &str, name: &str) -> String {
    if id.starts_with("double_pinyin") {
        "双".to_owned()
    } else if id.contains("pinyin") {
        "拼".to_owned()
    } else {
        name.chars().next().map(String::from).unwrap_or_default()
    }
}

impl State {
    /// The session of `ctx`, made when Rime is ready.
    fn session(&mut self, ctx: ContextId) -> Option<SessionId> {
        if !self.deployed {
            return None;
        }
        let rime = self.rime.as_ref()?;
        let field = self.fields.entry(ctx).or_default();
        if let Some(session) = field.session.filter(|&s| rime.session_exists(s)) {
            return Some(session);
        }
        let session = rime.create_session()?;
        if let Some(schema) = &self.schema {
            rime.select_schema(session, schema);
        }
        rime.set_option(session, "ascii_mode", self.ascii);
        field.session = Some(session);
        Some(session)
    }

    /// What Rime has for the field after it handled something.
    fn effects(&self, ctx: ContextId, session: SessionId) -> Effects {
        let Some(rime) = &self.rime else {
            return Effects::default();
        };
        let commit = rime.take_commit(session);
        let composing = rime.composing(session);
        let client_preedit = self.fields.get(&ctx).is_some_and(|f| f.client_preedit);
        // Applications that leave the preedit to the input method see it in the window.
        let preedit = match &composing {
            Some(c) if client_preedit && !c.preedit.is_empty() => {
                let cursor = i32::try_from(c.cursor).unwrap_or(-1);
                Some(Preedit {
                    text: c.preedit.clone(),
                    cursor_begin: cursor,
                    cursor_end: cursor,
                })
            }
            _ => Some(Preedit::default()),
        };
        Effects { commit, preedit }
    }

    /// Follows Rime's English letters from `session` into every field, and the menu.
    fn follow_status(&mut self, session: SessionId) {
        let Some(rime) = &self.rime else { return };
        let Some(status) = rime.status(session) else {
            return;
        };
        if status.ascii_mode != self.ascii {
            self.ascii = status.ascii_mode;
            for field in self.fields.values() {
                if let Some(other) = field.session.filter(|&s| s != session) {
                    rime.set_option(other, "ascii_mode", self.ascii);
                }
            }
        }
        if !status.schema_id.is_empty() {
            self.schema = Some(status.schema_id.clone());
        }
        self.announce(Some(&status));
    }

    /// Read the actual session, rather than inferring a mode from its UI label.
    fn input_mode(&self, session: SessionId) -> Option<(String, &'static str)> {
        let status = self.rime.as_ref()?.status(session)?;
        Some((
            status.schema_id,
            if status.ascii_mode { "latin" } else { "native" },
        ))
    }

    fn report_input_mode(&self, ctx: &mut Context<'_>, session: SessionId) {
        if let Some((method, mode)) = self.input_mode(session) {
            ctx.report_input_mode(method, mode);
        }
    }

    fn report_focused_input_mode(&self, session: SessionId) {
        if let (Some(host), Some(ctx), Some((method, mode))) =
            (&self.host, self.focused, self.input_mode(session))
        {
            host.report_input_mode(ctx, method, mode);
        }
    }

    /// Tells the platform the status and the menu.
    fn announce(&mut self, status: Option<&rime::Status>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        if !self.deployed {
            host.set_status(Status {
                label: Some("…".to_owned()),
                icon: None,
                tooltip: Some(omime_proto::i18n::text("Rime is deploying…").to_owned()),
                input_mode: None,
            });
            return;
        }
        let label = if self.ascii { "A" } else { "中" };
        host.set_status(Status {
            label: Some(label.to_owned()),
            icon: None,
            tooltip: status
                .map(|s| names::display(&s.schema_name))
                .filter(|n| !n.is_empty()),
            input_mode: None,
        });
        let simplified = status.is_some_and(|s| s.simplified) || self.focused_option("zh_hans");
        let toggle = |id: &str, en: &str, checked: bool| Property {
            id: id.to_owned(),
            kind: PropertyKind::Toggle,
            label: omime_proto::i18n::text(en).to_owned(),
            checked,
            enabled: true,
            ..Property::default()
        };
        let menu = vec![
            toggle(
                "ascii_mode",
                omime_proto::i18n::message("English letters"),
                self.ascii,
            ),
            toggle(
                "full_shape",
                omime_proto::i18n::message("Full width"),
                status.is_some_and(|s| s.full_shape),
            ),
            toggle(
                "simplification",
                omime_proto::i18n::message("Simplified characters"),
                simplified,
            ),
            toggle(
                "ascii_punct",
                omime_proto::i18n::message("English punctuation"),
                status.is_some_and(|s| s.ascii_punct),
            ),
            Property {
                id: "separator".to_owned(),
                kind: PropertyKind::Separator,
                ..Property::default()
            },
            Property {
                id: "deploy".to_owned(),
                kind: PropertyKind::Action,
                label: omime_proto::i18n::text("Deploy again").to_owned(),
                icon: Some("view-refresh".to_owned()),
                enabled: true,
                ..Property::default()
            },
        ];
        if menu != self.menu {
            host.set_properties(&menu);
            self.menu = menu;
        }
    }

    fn focused_session(&self) -> Option<SessionId> {
        self.focused
            .and_then(|ctx| self.fields.get(&ctx))
            .and_then(|f| f.session)
    }

    fn focused_option(&self, name: &str) -> bool {
        match (&self.rime, self.focused_session()) {
            (Some(rime), Some(session)) => rime.option(session, name),
            _ => false,
        }
    }

    /// The schemas, as the platform's input methods.
    fn announce_schemas(&mut self) {
        let (Some(rime), Some(host)) = (&self.rime, &self.host) else {
            return;
        };
        let schemas = rime.schemas();
        let methods: Vec<InputMethodInfo> = schemas
            .iter()
            .map(|s| InputMethodInfo {
                id: s.id.clone(),
                name: names::display(&s.name),
                icon: None,
                label: Some(badge(&s.id, &s.name)),
                language: Some("zh".to_owned()),
            })
            .collect();
        let current = self
            .schema
            .clone()
            .filter(|id| schemas.iter().any(|s| &s.id == id))
            .or_else(|| schemas.first().map(|s| s.id.clone()))
            .unwrap_or_default();
        self.schema = (!current.is_empty()).then(|| current.clone());
        host.set_input_methods(&methods, current);
    }

    /// Librime finished deploying: sessions can be made now.
    fn deployed(&mut self) {
        if let Some(rime) = &self.rime {
            rime.join_deployment();
        }
        self.deployed = true;
        // Sessions of before the deployment have old data.
        let stale: Vec<SessionId> = self
            .fields
            .values_mut()
            .filter_map(|f| f.session.take())
            .collect();
        if let Some(rime) = &self.rime {
            for session in stale {
                rime.destroy_session(session);
            }
        }
        self.announce_schemas();
        if let Some(ctx) = self.focused
            && let Some(session) = self.session(ctx)
        {
            self.follow_status(session);
            self.report_focused_input_mode(session);
        } else {
            self.announce(None);
        }
    }
}

/// Pushes what the focused field composes to the window, and shows or hides it.
fn refresh(state: &mut State, ui: &slint::Weak<Candidates>) {
    let composing: Option<Composing> = state
        .focused_session()
        .and_then(|session| state.rime.as_ref()?.composing(session));
    let client_preedit = state
        .focused
        .and_then(|ctx| state.fields.get(&ctx))
        .is_some_and(|f| f.client_preedit);
    // Shown with candidates, or with a preedit the application does not show.
    let visible = composing
        .as_ref()
        .is_some_and(|c| !c.candidates.is_empty() || (!client_preedit && !c.preedit.is_empty()));
    if visible != state.shown
        && let Some(host) = &state.host
    {
        state.shown = visible;
        host.set_popup_visible(visible);
    }
    let Some(composing) = composing else {
        return;
    };
    let preedit = if client_preedit {
        String::new()
    } else {
        composing.preedit.clone()
    };
    let items: Vec<(String, String, String)> = composing
        .candidates
        .iter()
        .zip(&composing.labels)
        .map(|(c, label)| (label.clone(), c.text.clone(), c.comment.clone()))
        .collect();
    let highlighted = i32::try_from(composing.highlighted).unwrap_or(0);
    let (has_previous, has_next) = (composing.page > 0, !composing.last_page);
    // The window may already be gone during shutdown; nothing to update then.
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<Item> = items
            .into_iter()
            .map(|(label, text, comment)| Item {
                label: label.into(),
                text: text.into(),
                comment: comment.into(),
            })
            .collect();
        ui.set_items(ModelRc::new(VecModel::from(items)));
        ui.set_preedit(preedit.into());
        ui.set_highlighted(highlighted);
        ui.set_has_previous(has_previous);
        ui.set_has_next(has_next);
        let size = slint::LogicalSize::new(ui.get_wanted_width(), ui.get_wanted_height());
        ui.window().set_size(size);
    });
}

struct RimeEngine {
    state: Arc<Mutex<State>>,
    ui: slint::Weak<Candidates>,
}

impl RimeEngine {
    fn apply(ctx: &mut Context<'_>, effects: Effects) {
        if let Some(text) = effects.commit {
            ctx.commit(text);
        }
        match effects.preedit {
            Some(preedit) if preedit.text.is_empty() => ctx.clear_preedit(),
            Some(preedit) => ctx.set_preedit(preedit),
            None => {}
        }
    }

    fn remember(state: &mut State, ctx: &Context<'_>) {
        let field = state.fields.entry(ctx.id()).or_default();
        field.client_preedit = ctx.info().client_preedit;
    }
}

impl Engine for RimeEngine {
    fn input_modes(&self) -> Vec<String> {
        vec!["native".to_owned(), "latin".to_owned()]
    }

    fn registered(&mut self, host: &Host) {
        let mut state = lock(&self.state);
        state.host = Some(host.clone());
        if state.deployed {
            state.announce_schemas();
        }
        if let Some(session) = state.focused_session() {
            // Activate can arrive before Register's reply. The SDK cannot report
            // 1.2 actions until it knows the daemon version, so confirm again now.
            state.follow_status(session);
            state.report_focused_input_mode(session);
        } else {
            state.announce(None);
        }
    }

    fn select_input_method(&mut self, host: &Host, id: &str) {
        let mut state = lock(&self.state);
        state.schema = Some(id.to_owned());
        if let Some(rime) = &state.rime {
            for field in state.fields.values() {
                if let Some(session) = field.session
                    && rime
                        .status(session)
                        .is_none_or(|status| status.schema_id != id)
                {
                    // Selecting even the current schema resets librime's options
                    // and composition. Focus restoration can repeat this request.
                    rime.select_schema(session, id);
                }
            }
        }
        host.set_current_input_method(id);
        if let Some(session) = state.focused_session() {
            state.follow_status(session);
            state.report_focused_input_mode(session);
        }
        refresh(&mut state, &self.ui);
    }

    fn activate_property(&mut self, host: &Host, id: &str) {
        let mut state = lock(&self.state);
        if id == "deploy" {
            if let Some(rime) = &state.rime {
                rime.redeploy();
                state.deployed = !rime.deploying();
            }
            state.announce(None);
            let _ = host;
            return;
        }
        let Some(session) = state.focused_session() else {
            return;
        };
        let Some(rime) = &state.rime else { return };
        match id {
            "simplification" => {
                // luna_pinyin chooses among options rather than switching.
                let options = ["zh_hans", "zh_hant", "zh_hant_hk"];
                if options.iter().any(|o| rime.option(session, o)) {
                    let simplified = rime.option(session, "zh_hans");
                    rime.set_option(session, "zh_hans", !simplified);
                    rime.set_option(session, "zh_hant", simplified);
                    rime.set_option(session, "zh_hant_hk", false);
                } else {
                    let value = rime.option(session, "simplification");
                    rime.set_option(session, "simplification", !value);
                }
            }
            "ascii_mode" | "full_shape" | "ascii_punct" => {
                let value = rime.option(session, id);
                rime.set_option(session, id, !value);
            }
            _ => return,
        }
        state.follow_status(session);
        state.report_focused_input_mode(session);
        // Changing ascii_mode may clear or commit the current composition. Apply
        // librime's actual effects, as for a key, and update the candidate window.
        if let Some(ctx) = state.focused {
            let effects = state.effects(ctx, session);
            if let Some(text) = effects.commit {
                host.commit(ctx, text);
            }
            if let Some(preedit) = effects.preedit {
                host.set_preedit(ctx, preedit);
            }
        }
        refresh(&mut state, &self.ui);
    }

    fn activate(&mut self, ctx: &mut Context<'_>) {
        let mut state = lock(&self.state);
        Self::remember(&mut state, ctx);
        state.focused = Some(ctx.id());
        if let Some(session) = state.session(ctx.id()) {
            state.follow_status(session);
            state.report_input_mode(ctx, session);
        }
        refresh(&mut state, &self.ui);
    }

    fn deactivate(&mut self, ctx: &mut Context<'_>) {
        let mut state = lock(&self.state);
        if let Some(field) = state.fields.remove(&ctx.id())
            && let (Some(session), Some(rime)) = (field.session, &state.rime)
        {
            rime.destroy_session(session);
        }
        if state.focused == Some(ctx.id()) {
            state.focused = None;
        }
        refresh(&mut state, &self.ui);
    }

    fn update(&mut self, ctx: &mut Context<'_>) {
        let mut state = lock(&self.state);
        Self::remember(&mut state, ctx);
        refresh(&mut state, &self.ui);
    }

    fn reset(&mut self, ctx: &mut Context<'_>) {
        let mut state = lock(&self.state);
        if let Some(session) = state.fields.get(&ctx.id()).and_then(|f| f.session)
            && let Some(rime) = &state.rime
        {
            rime.clear(session);
            state.follow_status(session);
            state.report_input_mode(ctx, session);
        }
        ctx.clear_preedit();
        refresh(&mut state, &self.ui);
    }

    fn set_input_mode(&mut self, ctx: &mut Context<'_>, mode: &str) {
        let ascii = match mode {
            "native" => false,
            "latin" => true,
            _ => return,
        };
        let mut state = lock(&self.state);
        Self::remember(&mut state, ctx);
        if !state.deployed {
            // Keep the requested mode for the session created after deployment.
            // deployed() confirms it only after librime has actually applied it.
            state.ascii = ascii;
            return;
        }
        let Some(session) = state.session(ctx.id()) else {
            return;
        };
        let Some(rime) = &state.rime else { return };
        rime.set_option(session, "ascii_mode", ascii);
        Self::apply(ctx, state.effects(ctx.id(), session));
        state.follow_status(session);
        state.report_input_mode(ctx, session);
        refresh(&mut state, &self.ui);
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        let mut state = lock(&self.state);
        Self::remember(&mut state, ctx);
        state.focused = Some(ctx.id());
        // Until Rime has deployed, keys go to the application as they are.
        let Some(session) = state.session(ctx.id()) else {
            return false;
        };
        let Some(rime) = &state.rime else {
            return false;
        };
        let handled =
            rime.process_key(session, key.keysym, rime::mask(key.modifiers, key.released));
        let effects = state.effects(ctx.id(), session);
        Self::apply(ctx, effects);
        state.follow_status(session);
        state.report_input_mode(ctx, session);
        refresh(&mut state, &self.ui);
        handled
    }
}

/// Clicks and the wheel in the window: Rime picks or turns the page in the focused
/// field.
fn on_window(
    state: &Arc<Mutex<State>>,
    ui: &slint::Weak<Candidates>,
    act: impl FnOnce(&Rime, SessionId),
) {
    let mut state = lock(state);
    let (Some(ctx), Some(session)) = (state.focused, state.focused_session()) else {
        return;
    };
    let Some(rime) = &state.rime else { return };
    act(rime, session);
    let effects = state.effects(ctx, session);
    if let Some(host) = &state.host {
        if let Some(text) = effects.commit {
            host.commit(ctx, text);
        }
        if let Some(preedit) = effects.preedit {
            host.set_preedit(ctx, preedit);
        }
    }
    state.follow_status(session);
    state.report_focused_input_mode(session);
    refresh(&mut state, ui);
}

fn main() -> std::process::ExitCode {
    let result = if std::env::args().nth(1).as_deref() == Some("--settings") {
        settings::run()
    } else {
        run()
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-rime: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ui = Candidates::new()?;
    let style = style::Style::load();
    let look = ui.global::<Look<'_>>();
    look.set_vertical(style.layout == style::Layout::Vertical);
    look.set_appearance(match style.appearance {
        style::Appearance::System => 0,
        style::Appearance::Light => 1,
        style::Appearance::Dark => 2,
    });
    look.set_font_size(f32::from(u16::try_from(style.font_size).unwrap_or(18)));
    let state = Arc::new(Mutex::new(State::default()));
    let result = run_engine(&ui, &state);
    // The SDK, UI callbacks and notice thread also own the state. Finalize under
    // their lock on every return path, before C++ static teardown at process exit.
    lock(&state).rime = None;
    result
}

fn run_engine(
    ui: &Candidates,
    state: &Arc<Mutex<State>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Start Rime first: a deployment can take a while, and it runs in its own thread.
    let (notices, noticed) = mpsc::channel();
    match (data::shared_dir(), data::user_dir()) {
        (Some(shared), Some(user)) => {
            let started = omime_proto::paths::ensure_private_dir(&user)
                .map_err(|e| {
                    omime_proto::tr!(
                        "cannot make {arg0} private: {arg1}",
                        arg0 = user.display(),
                        arg1 = e
                    )
                })
                .and_then(|()| {
                    if let Err(e) = data::customize(&shared, &user) {
                        eprintln!(
                            "omime-rime: cannot write oh-my-ime's defaults into {}: {e}",
                            user.display()
                        );
                    }
                    Rime::start(&shared, &user, notices)
                });
            match started {
                Ok(rime) => {
                    eprintln!(
                        "omime-rime: librime {}, data in {} and {}",
                        rime.version(),
                        shared.display(),
                        user.display()
                    );
                    let mut state = lock(state);
                    state.deployed = !rime.deploying();
                    state.rime = Some(rime);
                }
                Err(e) => eprintln!("omime-rime: {e}; keys pass through"),
            }
        }
        (None, _) => eprintln!(
            "omime-rime: no Rime data (rime-data, or ${}); keys pass through",
            data::SHARED_DATA_ENV
        ),
        (_, None) => eprintln!("omime-rime: no home directory; keys pass through"),
    }

    let engine = RimeEngine {
        state: state.clone(),
        ui: ui.as_weak(),
    };
    let (host, engine_thread) = omime_sdk::spawn(engine)?;

    // Librime's notices, about deploying.
    let deploying = state.clone();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("rime-notices".to_owned())
        .spawn(move || {
            for notice in noticed {
                let mut state = lock(&deploying);
                match notice {
                    Notice::Deploying => {
                        state.deployed = false;
                        state.announce(None);
                    }
                    Notice::Deployed { ok } => {
                        if !ok {
                            eprintln!("omime-rime: deploying failed; see the messages above");
                        }
                        state.deployed();
                        refresh(&mut state, &weak);
                    }
                }
            }
        })?;

    let picks = state.clone();
    let weak = ui.as_weak();
    ui.on_picked(move |i| {
        let Ok(i) = usize::try_from(i) else { return };
        on_window(&picks, &weak, |rime, session| {
            rime.pick(session, i);
        });
    });
    let turns = state.clone();
    let weak = ui.as_weak();
    ui.on_turn(move |back| {
        on_window(&turns, &weak, |rime, session| {
            rime.turn_page(session, back);
        });
    });

    // When the platform stops the engine, close the window too.
    std::thread::spawn(move || {
        if let Ok(Err(e)) = engine_thread.join() {
            eprintln!("omime-rime: {e}");
        }
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    // The window is always "shown" for the toolkit; the platform decides when the user
    // sees it.
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    drop(host);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_have_badges() {
        assert_eq!(badge("double_pinyin_flypy", "小鶴雙拼"), "双");
        assert_eq!(badge("luna_pinyin", "朙月拼音"), "拼");
        assert_eq!(badge("cangjie5", "倉頡五代"), "倉");
    }
}
