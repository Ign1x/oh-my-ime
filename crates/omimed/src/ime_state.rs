//! Double-buffered input method state (`zwp_input_method_v2`).
//!
//! The compositor sends `activate`, `deactivate`, `surrounding_text`,
//! `text_change_cause` and `content_type` as pending state and applies all of it on
//! `done`. This module implements those rules without any I/O so they can be tested.

use omime_proto::{ContentHints, ContentPurpose, ContentType};

/// Surrounding text of the focused field. `cursor` and `anchor` are byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurroundingText {
    pub text: String,
    pub cursor: u32,
    pub anchor: u32,
}

/// Why the surrounding text changed (`zwp_text_input_v3.change_cause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChangeCause {
    /// The change was caused by the input method.
    #[default]
    InputMethod,
    /// Something else changed the text (typing, pasting, the application itself).
    Other,
}

impl ChangeCause {
    pub fn from_raw(raw: u32) -> Self {
        if raw == 0 {
            Self::InputMethod
        } else {
            Self::Other
        }
    }
}

/// The applied state of the input method.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Current {
    pub active: bool,
    pub surrounding: Option<SurroundingText>,
    pub cause: ChangeCause,
    pub content_type: ContentType,
}

#[derive(Debug, Clone, Default)]
struct Pending {
    /// `Some(true)` after `activate`, `Some(false)` after `deactivate`.
    activation: Option<bool>,
    surrounding: Option<SurroundingText>,
    cause: Option<ChangeCause>,
    content_type: Option<ContentType>,
}

/// What a `done` event changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// The serial to use in the next `zwp_input_method_v2.commit`.
    pub serial: u32,
    pub was_active: bool,
    pub is_active: bool,
    /// The input method was (re-)activated, which resets all text input state.
    pub activated: bool,
    /// The application reported that something other than the input method changed
    /// the text.
    pub changed_by_other: bool,
}

/// Pending and current input method state of one seat.
#[derive(Debug, Default)]
pub struct ImState {
    pending: Pending,
    current: Current,
    done_count: u32,
}

impl ImState {
    pub fn current(&self) -> &Current {
        &self.current
    }

    /// Number of `done` events received, which is the serial for `commit`.
    pub fn serial(&self) -> u32 {
        self.done_count
    }

    pub fn activate(&mut self) {
        // activate resets everything sent before it.
        self.pending = Pending {
            activation: Some(true),
            ..Pending::default()
        };
    }

    pub fn deactivate(&mut self) {
        self.pending.activation = Some(false);
    }

    pub fn surrounding_text(&mut self, text: String, cursor: u32, anchor: u32) {
        self.pending.surrounding = Some(SurroundingText {
            text,
            cursor,
            anchor,
        });
    }

    pub fn text_change_cause(&mut self, raw: u32) {
        self.pending.cause = Some(ChangeCause::from_raw(raw));
    }

    pub fn content_type(&mut self, hints: u32, purpose: u32) {
        self.pending.content_type = Some(ContentType {
            purpose: ContentPurpose::from_raw(purpose),
            hints: ContentHints(hints),
        });
    }

    /// Applies the pending state.
    pub fn done(&mut self) -> Applied {
        self.done_count = self.done_count.wrapping_add(1);
        let pending = std::mem::take(&mut self.pending);
        let was_active = self.current.active;
        let activated = pending.activation == Some(true);
        if activated {
            self.current = Current {
                active: true,
                ..Current::default()
            };
        } else if pending.activation == Some(false) {
            self.current.active = false;
        }
        if let Some(surrounding) = pending.surrounding {
            self.current.surrounding = Some(surrounding);
        }
        let changed_by_other = pending.cause == Some(ChangeCause::Other);
        if let Some(cause) = pending.cause {
            self.current.cause = cause;
        }
        if let Some(content_type) = pending.content_type {
            self.current.content_type = content_type;
        }
        Applied {
            serial: self.done_count,
            was_active,
            is_active: self.current.active,
            activated,
            changed_by_other,
        }
    }

    /// Forgets everything, e.g. after the input method object was recreated.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_applied_on_done_only() {
        let mut im = ImState::default();
        im.activate();
        im.content_type(0, 8);
        im.surrounding_text("hello".into(), 5, 5);
        assert!(!im.current().active);

        let applied = im.done();
        assert_eq!(
            applied,
            Applied {
                serial: 1,
                was_active: false,
                is_active: true,
                activated: true,
                changed_by_other: false,
            }
        );
        assert!(im.current().content_type.is_sensitive());
        assert_eq!(im.current().surrounding.as_ref().unwrap().text, "hello");
    }

    #[test]
    fn activate_resets_previous_state() {
        let mut im = ImState::default();
        im.activate();
        im.content_type(0, 8);
        im.surrounding_text("secret".into(), 6, 6);
        im.done();

        im.activate();
        im.content_type(0, 0);
        let applied = im.done();
        assert!(applied.activated && applied.was_active && applied.is_active);
        assert!(!im.current().content_type.is_sensitive());
        assert_eq!(im.current().surrounding, None);
        assert_eq!(applied.serial, 2);
    }

    #[test]
    fn updates_keep_unchanged_fields() {
        let mut im = ImState::default();
        im.activate();
        im.content_type(0x200, 13);
        im.done();

        im.surrounding_text("ab".into(), 1, 1);
        im.text_change_cause(1);
        let applied = im.done();
        assert!(!applied.activated && applied.is_active && applied.changed_by_other);
        assert_eq!(im.current().content_type.purpose, ContentPurpose::Terminal);
        assert_eq!(im.current().cause, ChangeCause::Other);
    }

    #[test]
    fn deactivate_keeps_serial_counting() {
        let mut im = ImState::default();
        im.activate();
        im.done();
        im.deactivate();
        let applied = im.done();
        assert!(applied.was_active && !applied.is_active);
        assert_eq!(im.serial(), 2);
    }
}
