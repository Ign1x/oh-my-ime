//! Echo — a tiny oh-my-ime engine, used by the end-to-end tests and as a tutorial.
//!
//! Letters are collected in an inline preedit. Space, Return or any other key that is
//! not a letter commits the word in UPPERCASE and is then passed on to the application.
//! Backspace edits the preedit and Escape cancels it.

use omime_sdk::{Context, Engine, KeyEvent, Preedit, Status, modifiers};

/// Keysyms used below (see xkbcommon-keysyms.h).
mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const ESCAPE: u32 = 0xff1b;
}

#[derive(Debug, Default)]
struct Echo {
    word: String,
}

impl Echo {
    fn show(&self, ctx: &mut Context<'_>) {
        if self.word.is_empty() {
            ctx.clear_preedit();
        } else {
            ctx.set_preedit(Preedit::with_cursor_at_end(self.word.clone()));
        }
    }

    fn flush(&mut self, ctx: &mut Context<'_>) {
        if !self.word.is_empty() {
            ctx.commit(std::mem::take(&mut self.word).to_uppercase());
        }
    }
}

impl Engine for Echo {
    fn activate(&mut self, ctx: &mut Context<'_>) {
        self.word.clear();
        ctx.host().set_status(Status {
            label: Some("ECHO".into()),
            icon: None,
            tooltip: Some("Echo: commits words in UPPERCASE".into()),
        });
    }

    fn deactivate(&mut self, _ctx: &mut Context<'_>) {
        self.word.clear();
    }

    fn reset(&mut self, _ctx: &mut Context<'_>) {
        self.word.clear();
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        if key.released {
            return false;
        }
        let shortcut = key.modifiers & (modifiers::CONTROL | modifiers::ALT | modifiers::SUPER);
        let letter = key
            .text
            .chars()
            .next()
            .filter(|c| key.text.chars().count() == 1 && c.is_alphabetic());

        match (letter, key.keysym) {
            (Some(c), _) if shortcut == 0 => {
                self.word.push(c);
                self.show(ctx);
                true
            }
            (_, keysym::BACKSPACE) if !self.word.is_empty() => {
                self.word.pop();
                self.show(ctx);
                true
            }
            (_, keysym::ESCAPE) if !self.word.is_empty() => {
                self.word.clear();
                self.show(ctx);
                true
            }
            _ => {
                // Commit what we have, then let the application handle the key.
                self.flush(ctx);
                false
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    match omime_sdk::run(Echo::default()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-echo: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
