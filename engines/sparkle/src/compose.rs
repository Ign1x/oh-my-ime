//! The composing state machine, independent of any UI.
//!
//! Typing `:` starts a query; letters extend it and show matching emoji. Tab and the
//! arrow keys move the selection, Space, Return or a click inserts it, Escape keeps the
//! typed text as it is. Everything else ends the query and reaches the application.

use omime_sdk::{KeyEvent, modifiers};

use crate::search::{Candidate, Index};

/// Number of candidates shown at once.
pub const PAGE: usize = 8;

mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const LEFT: u32 = 0xff51;
    pub const RIGHT: u32 = 0xff53;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const ISO_LEFT_TAB: u32 = 0xfe20;
    pub const SPACE: u32 = 0x20;
}

/// What a key did to the text field.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Text to insert.
    pub commit: Option<String>,
    /// New preedit; `Some("")` removes it.
    pub preedit: Option<String>,
    /// Whether the key was consumed.
    pub handled: bool,
}

/// What the popup should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub query: String,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    pub visible: bool,
}

#[derive(Debug, Default)]
pub struct Composer {
    /// `Some` while composing; the text after the colon.
    query: Option<String>,
    candidates: Vec<Candidate>,
    selected: usize,
}

fn is_query_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '+' | '-')
}

impl Composer {
    pub fn view(&self) -> View {
        View {
            query: self.query.clone().unwrap_or_default(),
            candidates: self.candidates.clone(),
            selected: self.selected,
            visible: self.query.as_deref().is_some_and(|q| !q.is_empty())
                && !self.candidates.is_empty(),
        }
    }

    #[cfg(test)]
    pub fn is_composing(&self) -> bool {
        self.query.is_some()
    }

    /// Forgets the query without touching the text field.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn preedit(&self) -> String {
        format!(":{}", self.query.as_deref().unwrap_or_default())
    }

    fn update(&mut self, index: &Index) {
        let query = self.query.as_deref().unwrap_or_default();
        self.candidates = index.search(query, PAGE);
        self.selected = 0;
    }

    /// Ends the query and gives the typed text back as plain text.
    fn give_back(&mut self) -> Effects {
        let text = self.preedit();
        self.reset();
        Effects {
            commit: Some(text),
            preedit: Some(String::new()),
            handled: false,
        }
    }

    /// Ends the query by inserting candidate `i`.
    pub fn pick(&mut self, i: usize) -> Option<Effects> {
        let glyph = self.candidates.get(i)?.glyph;
        self.reset();
        Some(Effects {
            commit: Some(glyph.to_owned()),
            preedit: Some(String::new()),
            handled: true,
        })
    }

    pub fn key(&mut self, index: &Index, key: &KeyEvent) -> Effects {
        if key.released {
            return Effects::default();
        }
        let shortcut = key.modifiers & (modifiers::CONTROL | modifiers::ALT | modifiers::SUPER);
        let Some(query) = &mut self.query else {
            if key.text == ":" && shortcut == 0 {
                self.query = Some(String::new());
                return Effects {
                    preedit: Some(":".into()),
                    handled: true,
                    ..Effects::default()
                };
            }
            return Effects::default();
        };
        if shortcut != 0 {
            return self.give_back();
        }

        let typed = key
            .text
            .chars()
            .next()
            .filter(|_| key.text.chars().count() == 1);
        match key.keysym {
            keysym::BACKSPACE => {
                if query.pop().is_none() {
                    // Removing the colon ends the query.
                    self.reset();
                    return Effects {
                        preedit: Some(String::new()),
                        handled: true,
                        ..Effects::default()
                    };
                }
                self.update(index);
                self.consumed_with_preedit()
            }
            keysym::ESCAPE => {
                let mut effects = self.give_back();
                effects.handled = true;
                effects
            }
            keysym::TAB | keysym::RIGHT if !self.candidates.is_empty() => {
                self.selected = (self.selected + 1) % self.candidates.len();
                self.consumed()
            }
            keysym::ISO_LEFT_TAB | keysym::LEFT if !self.candidates.is_empty() => {
                self.selected = (self.selected + self.candidates.len() - 1) % self.candidates.len();
                self.consumed()
            }
            keysym::RETURN | keysym::KP_ENTER | keysym::SPACE if !self.candidates.is_empty() => {
                let selected = self.selected;
                self.pick(selected).unwrap_or_default()
            }
            _ => match typed {
                // A new colon starts a new query; the old one stays as plain text.
                Some(':') => {
                    let mut effects = self.give_back();
                    self.query = Some(String::new());
                    effects.preedit = Some(":".into());
                    effects.handled = true;
                    effects
                }
                Some(c) if is_query_char(c) => {
                    query.push(c);
                    self.update(index);
                    self.consumed_with_preedit()
                }
                // Anything else ends the query; the key then reaches the application.
                _ => self.give_back(),
            },
        }
    }

    fn consumed(&self) -> Effects {
        Effects {
            handled: true,
            ..Effects::default()
        }
    }

    fn consumed_with_preedit(&self) -> Effects {
        Effects {
            preedit: Some(self.preedit()),
            handled: true,
            ..Effects::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str, keysym: u32) -> KeyEvent {
        KeyEvent {
            keysym,
            keycode: 0,
            modifiers: 0,
            time: 0,
            text: text.into(),
            released: false,
            repeat: false,
        }
    }

    fn typed(c: char) -> KeyEvent {
        key(&c.to_string(), c as u32)
    }

    fn type_str(composer: &mut Composer, index: &Index, s: &str) -> Effects {
        let mut last = Effects::default();
        for c in s.chars() {
            last = composer.key(index, &typed(c));
        }
        last
    }

    #[test]
    fn plain_typing_is_untouched() {
        let index = Index::new();
        let mut composer = Composer::default();
        assert_eq!(type_str(&mut composer, &index, "hello"), Effects::default());
        assert!(!composer.is_composing());
    }

    #[test]
    fn a_shortcode_becomes_an_emoji() {
        let index = Index::new();
        let mut composer = Composer::default();
        let effects = type_str(&mut composer, &index, ":rocket");
        assert_eq!(effects.preedit.as_deref(), Some(":rocket"));
        assert!(composer.view().visible);
        let effects = composer.key(&index, &key(" ", keysym::SPACE));
        assert_eq!(effects.commit.as_deref(), Some("🚀"));
        assert!(effects.handled);
        assert!(!composer.view().visible);
    }

    #[test]
    fn tab_moves_the_selection() {
        let index = Index::new();
        let mut composer = Composer::default();
        type_str(&mut composer, &index, ":heart");
        let second = composer.view().candidates[1].glyph;
        composer.key(&index, &key("\t", keysym::TAB));
        assert_eq!(composer.view().selected, 1);
        let effects = composer.key(&index, &key("\r", keysym::RETURN));
        assert_eq!(effects.commit.as_deref(), Some(second));
    }

    #[test]
    fn escape_and_punctuation_keep_the_text() {
        let index = Index::new();
        let mut composer = Composer::default();
        type_str(&mut composer, &index, ":smi");
        let effects = composer.key(&index, &key("\u{1b}", keysym::ESCAPE));
        assert_eq!(effects.commit.as_deref(), Some(":smi"));
        assert!(effects.handled);

        // "Note: x" — the colon is followed by a space and no candidates.
        let effects = type_str(&mut composer, &index, ":");
        assert!(effects.handled);
        let effects = composer.key(&index, &key(" ", keysym::SPACE));
        assert_eq!(effects.commit.as_deref(), Some(":"));
        assert!(!effects.handled);
    }

    #[test]
    fn a_second_colon_starts_a_new_query() {
        let index = Index::new();
        let mut composer = Composer::default();
        type_str(&mut composer, &index, ":nope");
        let effects = composer.key(&index, &typed(':'));
        assert_eq!(effects.commit.as_deref(), Some(":nope"));
        assert_eq!(effects.preedit.as_deref(), Some(":"));
        assert!(effects.handled && composer.is_composing());
    }

    #[test]
    fn backspace_edits_and_finally_removes_the_colon() {
        let index = Index::new();
        let mut composer = Composer::default();
        type_str(&mut composer, &index, ":ab");
        let bs = key("\u{8}", keysym::BACKSPACE);
        assert_eq!(composer.key(&index, &bs).preedit.as_deref(), Some(":a"));
        assert_eq!(composer.key(&index, &bs).preedit.as_deref(), Some(":"));
        let effects = composer.key(&index, &bs);
        assert_eq!(effects.preedit.as_deref(), Some(""));
        assert!(effects.handled && !composer.is_composing());
    }
}
