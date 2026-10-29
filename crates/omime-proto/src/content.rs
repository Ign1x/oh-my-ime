//! Content type of the focused text field.
//!
//! The values mirror `zwp_text_input_v3` (and therefore `zwp_input_method_v2`), which
//! is what Wayland compositors forward to the input method. Other frontends (XIM, IM
//! modules) map their own notions onto these types.

/// Purpose of the focused text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContentPurpose {
    #[default]
    Normal,
    Alpha,
    Digits,
    Number,
    Phone,
    Url,
    Email,
    Name,
    Password,
    Pin,
    Date,
    Time,
    DateTime,
    Terminal,
    /// A value this version does not know about.
    Unknown(u32),
}

impl ContentPurpose {
    /// Converts the raw `zwp_text_input_v3.content_purpose` value.
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Normal,
            1 => Self::Alpha,
            2 => Self::Digits,
            3 => Self::Number,
            4 => Self::Phone,
            5 => Self::Url,
            6 => Self::Email,
            7 => Self::Name,
            8 => Self::Password,
            9 => Self::Pin,
            10 => Self::Date,
            11 => Self::Time,
            12 => Self::DateTime,
            13 => Self::Terminal,
            other => Self::Unknown(other),
        }
    }

    /// Returns the raw `zwp_text_input_v3.content_purpose` value.
    pub fn to_raw(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::Alpha => 1,
            Self::Digits => 2,
            Self::Number => 3,
            Self::Phone => 4,
            Self::Url => 5,
            Self::Email => 6,
            Self::Name => 7,
            Self::Password => 8,
            Self::Pin => 9,
            Self::Date => 10,
            Self::Time => 11,
            Self::DateTime => 12,
            Self::Terminal => 13,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Hints about the focused text field (`zwp_text_input_v3.content_hint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ContentHints(pub u32);

impl ContentHints {
    pub const NONE: Self = Self(0x0);
    pub const COMPLETION: Self = Self(0x1);
    pub const SPELLCHECK: Self = Self(0x2);
    pub const AUTO_CAPITALIZATION: Self = Self(0x4);
    pub const LOWERCASE: Self = Self(0x8);
    pub const UPPERCASE: Self = Self(0x10);
    pub const TITLECASE: Self = Self(0x20);
    pub const HIDDEN_TEXT: Self = Self(0x40);
    pub const SENSITIVE_DATA: Self = Self(0x80);
    pub const LATIN: Self = Self(0x100);
    pub const MULTILINE: Self = Self(0x200);

    /// Returns whether all bits of `other` are set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for ContentHints {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Purpose and hints of the focused text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ContentType {
    pub purpose: ContentPurpose,
    pub hints: ContentHints,
}

impl ContentType {
    /// Returns whether the field holds secrets.
    ///
    /// Keys typed into such fields must never reach an engine, and the daemon does not
    /// even hold a keyboard grab while one is focused.
    pub fn is_sensitive(self) -> bool {
        matches!(self.purpose, ContentPurpose::Password | ContentPurpose::Pin)
            || self.hints.contains(ContentHints::SENSITIVE_DATA)
            || self.hints.contains(ContentHints::HIDDEN_TEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_round_trips_through_raw_values() {
        for raw in 0..=20 {
            assert_eq!(ContentPurpose::from_raw(raw).to_raw(), raw);
        }
        assert_eq!(ContentPurpose::from_raw(8), ContentPurpose::Password);
    }

    #[test]
    fn sensitivity_covers_purposes_and_hints() {
        let normal = ContentType::default();
        assert!(!normal.is_sensitive());

        let password = ContentType {
            purpose: ContentPurpose::Password,
            ..normal
        };
        assert!(password.is_sensitive());

        let pin = ContentType {
            purpose: ContentPurpose::Pin,
            ..normal
        };
        assert!(pin.is_sensitive());

        let hidden = ContentType {
            hints: ContentHints::COMPLETION | ContentHints::HIDDEN_TEXT,
            ..normal
        };
        assert!(hidden.is_sensitive());

        let secret = ContentType {
            hints: ContentHints::SENSITIVE_DATA,
            ..normal
        };
        assert!(secret.is_sensitive());

        let terminal = ContentType {
            purpose: ContentPurpose::Terminal,
            hints: ContentHints::MULTILINE,
        };
        assert!(!terminal.is_sensitive());
    }
}
