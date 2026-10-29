//! Sparkle — an oh-my-ime engine that draws its own UI.
//!
//! Type `:` and a few letters of an emoji's shortcode (`:rocket`, `:heart`). A deck of
//! matching emoji appears next to the text cursor, drawn with Slint: rounded, translucent,
//! with a sliding selection and an arrow that points at the cursor. Tab or the arrow
//! keys choose, Space or Return inserts, a click works too, Escape keeps the text.
//!
//! The engine logic runs on the SDK's thread; the UI owns the main thread. The window is
//! an ordinary frameless Slint window: oh-my-ime's UI bridge turns it into a popup.

mod compose;
mod search;

use std::sync::{Arc, Mutex};

use compose::{Composer, Effects, PAGE, View};
use omime_sdk::{Context, ContextId, Engine, Host, KeyEvent, Preedit};
use search::Index;
use slint::{ComponentHandle, ModelRc, VecModel};

slint::slint! {
    export struct Card {
        glyph: string,
        label: string,
    }

    export component Popup inherits Window {
        title: "Sparkle";
        no-frame: true;
        background: transparent;
        width: 584px;
        height: 150px;

        in property <[Card]> cards;
        in property <int> selected;
        in property <string> query;
        // The text cursor, relative to this window, as reported by the platform.
        in property <length> cursor-x: 48px;
        // The popup sits above the text: point the arrow down.
        in property <bool> above-text: false;

        callback picked(int);

        property <length> card-width: 68px;
        property <length> pad: 12px;
        property <length> panel-top: root.above-text ? 4px : 14px;

        panel := Rectangle {
            x: 4px;
            y: root.panel-top;
            width: parent.width - 8px;
            height: parent.height - 18px;
            border-radius: 18px;
            background: @linear-gradient(115deg, #1f1b46 0%, #352a73 55%, #6f45c9 100%);
            border-width: 1px;
            border-color: #ffffff26;
            drop-shadow-blur: 8px;
            drop-shadow-color: #0000005a;
            drop-shadow-offset-y: 2px;

            Text {
                x: 18px;
                y: 10px;
                text: root.query;
                color: #ffffff;
                font-size: 15px;
                font-weight: 700;
            }
            Text {
                x: parent.width - self.width - 18px;
                y: 13px;
                text: "⇥ choose   ⏎ insert   esc keep text";
                color: #d9ccff99;
                font-size: 10px;
            }

            highlight := Rectangle {
                x: root.pad + root.selected * root.card-width;
                y: 36px;
                width: root.card-width - 6px;
                height: 86px;
                border-radius: 14px;
                background: #ffffff30;
                border-width: 1px;
                border-color: #ffffffb0;
                opacity: root.cards.length > 0 ? 1 : 0;
                animate x { duration: 150ms; easing: cubic-bezier(0.33, 1, 0.68, 1); }
                animate opacity { duration: 120ms; }
            }

            for card[i] in root.cards: Rectangle {
                x: root.pad + i * root.card-width;
                y: 36px;
                width: root.card-width - 6px;
                height: 86px;
                border-radius: 14px;
                background: touch.has-hover ? #ffffff1c : transparent;
                animate background { duration: 100ms; }

                Text {
                    y: 8px;
                    width: parent.width;
                    horizontal-alignment: center;
                    text: card.glyph;
                    // Without this, emoji with a text presentation (❤, ♥) come out as
                    // plain black glyphs from the UI font.
                    font-family: "Noto Color Emoji";
                    font-size: i == root.selected ? 38px : 34px;
                    animate font-size { duration: 150ms; easing: ease-out-back; }
                }
                Text {
                    x: 4px;
                    y: 62px;
                    width: parent.width - 8px;
                    horizontal-alignment: center;
                    overflow: elide;
                    text: card.label;
                    color: i == root.selected ? #ffffff : #e2d9ffb0;
                    font-size: 10px;
                }
                touch := TouchArea {
                    mouse-cursor: pointer;
                    clicked => { root.picked(i); }
                }
            }
        }

        // An arrow from the deck to the text cursor.
        Path {
            x: max(20px, min(root.width - 44px, root.cursor-x - 12px));
            y: root.above-text ? root.height - 15px : 4px;
            width: 24px;
            height: 11px;
            commands: root.above-text ? "M 0 0 L 12 11 L 24 0 Z" : "M 0 11 L 12 0 L 24 11 Z";
            fill: root.above-text ? #5d3db8 : #241f50;
            animate x { duration: 160ms; easing: cubic-bezier(0.33, 1, 0.68, 1); }
        }
    }
}

/// State shared by the engine thread and the UI thread.
#[derive(Default)]
struct Shared {
    composer: Composer,
    /// The focused text field, while Sparkle is the active engine.
    ctx: Option<ContextId>,
    /// The text cursor relative to the window: `(x, y, width, height)`.
    cursor: Option<(i32, i32, i32, i32)>,
    /// Whether we asked the platform to show the window.
    shown: bool,
}

struct Sparkle {
    shared: Arc<Mutex<Shared>>,
    index: Arc<Index>,
    ui: slint::Weak<Popup>,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pushes the current state to the window and tells the platform whether to show it.
fn refresh(shared: &mut Shared, host: &Host, ui: &slint::Weak<Popup>) {
    let view: View = shared.composer.view();
    if view.visible != shared.shown {
        shared.shown = view.visible;
        host.set_popup_visible(view.visible);
    }
    let cursor = shared.cursor;
    let cards: Vec<(String, String)> = view
        .candidates
        .iter()
        .map(|c| (c.glyph.to_owned(), format!(":{}", c.label)))
        .collect();
    let selected = view.selected.min(PAGE) as i32;
    let query = format!(":{}", view.query);
    // The window may already be gone during shutdown; nothing to update then.
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let cards: Vec<Card> = cards
            .into_iter()
            .map(|(glyph, label)| Card {
                glyph: glyph.into(),
                label: label.into(),
            })
            .collect();
        ui.set_cards(ModelRc::new(VecModel::from(cards)));
        ui.set_selected(selected);
        ui.set_query(query.into());
        if let Some((x, y, width, _)) = cursor {
            ui.set_cursor_x((x + width / 2) as f32);
            // A cursor below the window's top edge means the window is above the text.
            ui.set_above_text(y > 0);
        }
    });
}

impl Sparkle {
    fn apply(ctx: &mut Context<'_>, effects: Effects) -> bool {
        if let Some(text) = effects.commit {
            ctx.commit(text);
        }
        match effects.preedit.as_deref() {
            Some("") => ctx.clear_preedit(),
            Some(text) => ctx.set_preedit(Preedit::with_cursor_at_end(text)),
            None => {}
        }
        effects.handled
    }
}

impl Engine for Sparkle {
    fn activate(&mut self, ctx: &mut Context<'_>) {
        let mut shared = lock(&self.shared);
        shared.ctx = Some(ctx.id());
        shared.composer.reset();
        shared.cursor = ctx.info().cursor_rect;
        refresh(&mut shared, ctx.host(), &self.ui);
        ctx.host().set_status(omime_sdk::Status {
            label: Some("✨".into()),
            icon: Some("face-smile".into()),
            tooltip: Some("Sparkle: type :shortcode for emoji".into()),
        });
    }

    fn deactivate(&mut self, ctx: &mut Context<'_>) {
        let mut shared = lock(&self.shared);
        shared.ctx = None;
        shared.composer.reset();
        refresh(&mut shared, ctx.host(), &self.ui);
    }

    fn update(&mut self, ctx: &mut Context<'_>) {
        let mut shared = lock(&self.shared);
        shared.cursor = ctx.info().cursor_rect;
        refresh(&mut shared, ctx.host(), &self.ui);
    }

    fn reset(&mut self, ctx: &mut Context<'_>) {
        let mut shared = lock(&self.shared);
        shared.composer.reset();
        refresh(&mut shared, ctx.host(), &self.ui);
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        let mut shared = lock(&self.shared);
        let effects = shared.composer.key(&self.index, key);
        let handled = Self::apply(ctx, effects);
        refresh(&mut shared, ctx.host(), &self.ui);
        handled
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-sparkle: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ui = Popup::new()?;
    let shared = Arc::new(Mutex::new(Shared::default()));
    let engine = Sparkle {
        shared: shared.clone(),
        index: Arc::new(Index::new()),
        ui: ui.as_weak(),
    };
    let (host, engine_thread) = omime_sdk::spawn(engine)?;

    // Clicking a card inserts it.
    let click_host = host.clone();
    let weak = ui.as_weak();
    ui.on_picked(move |i| {
        let mut shared = lock(&shared);
        let Some(ctx) = shared.ctx else { return };
        let Ok(i) = usize::try_from(i) else { return };
        if let Some(effects) = shared.composer.pick(i)
            && let Some(text) = effects.commit
        {
            // A commit replaces the preedit, so the `:query` disappears with it.
            click_host.commit(ctx, text);
        }
        refresh(&mut shared, &click_host, &weak);
    });

    // When the platform stops the engine, close the window too.
    std::thread::spawn(move || {
        if let Ok(Err(e)) = engine_thread.join() {
            eprintln!("omime-sparkle: {e}");
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
