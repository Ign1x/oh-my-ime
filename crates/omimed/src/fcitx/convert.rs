//! Conversions between the Fcitx5 module protocol and omimed's types.
//!
//! The protocol counts surrounding text in characters but the preedit cursor in bytes;
//! omimed counts everything in bytes.

use omime_proto::engine::Preedit;
use omime_proto::{ContentHints, ContentPurpose, ContentType};

/// `fcitx::CapabilityFlag` bits this frontend reads.
pub mod capability {
    pub const PASSWORD: u64 = 1 << 3;
    pub const EMAIL: u64 = 1 << 7;
    pub const DIGIT: u64 = 1 << 8;
    pub const UPPERCASE: u64 = 1 << 9;
    pub const LOWERCASE: u64 = 1 << 10;
    pub const URL: u64 = 1 << 12;
    pub const DIALABLE: u64 = 1 << 13;
    pub const NUMBER: u64 = 1 << 14;
    pub const SPELL_CHECK: u64 = 1 << 16;
    pub const WORD_COMPLETION: u64 = 1 << 18;
    pub const UPPERCASE_WORDS: u64 = 1 << 19;
    pub const UPPERCASE_SENTENCES: u64 = 1 << 20;
    pub const ALPHA: u64 = 1 << 21;
    pub const NAME: u64 = 1 << 22;
    pub const TERMINAL: u64 = 1 << 32;
    pub const DATE: u64 = 1 << 33;
    pub const TIME: u64 = 1 << 34;
    pub const MULTILINE: u64 = 1 << 35;
    pub const SENSITIVE: u64 = 1 << 36;
    /// The client handles a key that was not consumed only after the text the key
    /// produced; see [`super::super::FcitxFrontend::finish_key`].
    pub const KEY_EVENT_ORDER_FIX: u64 = 1 << 37;
}

/// `fcitx::TextFormatFlag::Underline`.
const FORMAT_UNDERLINE: i32 = 1 << 3;

/// The content type an application announces through its capabilities.
pub fn content_type(caps: u64) -> ContentType {
    use capability as c;
    let has = |bit: u64| caps & bit != 0;
    let purpose = if has(c::PASSWORD) {
        ContentPurpose::Password
    } else if has(c::EMAIL) {
        ContentPurpose::Email
    } else if has(c::URL) {
        ContentPurpose::Url
    } else if has(c::DIALABLE) {
        ContentPurpose::Phone
    } else if has(c::NUMBER) {
        ContentPurpose::Number
    } else if has(c::DIGIT) {
        ContentPurpose::Digits
    } else if has(c::DATE) && has(c::TIME) {
        ContentPurpose::DateTime
    } else if has(c::DATE) {
        ContentPurpose::Date
    } else if has(c::TIME) {
        ContentPurpose::Time
    } else if has(c::TERMINAL) {
        ContentPurpose::Terminal
    } else if has(c::NAME) {
        ContentPurpose::Name
    } else if has(c::ALPHA) {
        ContentPurpose::Alpha
    } else {
        ContentPurpose::Normal
    };
    let hints = [
        (c::WORD_COMPLETION, ContentHints::COMPLETION),
        (c::SPELL_CHECK, ContentHints::SPELLCHECK),
        (c::UPPERCASE_SENTENCES, ContentHints::AUTO_CAPITALIZATION),
        (c::LOWERCASE, ContentHints::LOWERCASE),
        (c::UPPERCASE, ContentHints::UPPERCASE),
        (c::UPPERCASE_WORDS, ContentHints::TITLECASE),
        (c::MULTILINE, ContentHints::MULTILINE),
        (c::SENSITIVE, ContentHints::SENSITIVE_DATA),
    ]
    .into_iter()
    .filter(|(bit, _)| has(*bit))
    .fold(0, |hints, (_, hint)| hints | hint.0);
    ContentType {
        purpose,
        hints: ContentHints(hints),
    }
}

/// The byte offset of the `chars`th character of `text` (its end if it is shorter).
pub fn byte_offset(text: &str, chars: u32) -> u32 {
    let offset = text
        .char_indices()
        .nth(chars as usize)
        .map_or(text.len(), |(offset, _)| offset);
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// The number of characters in the first `bytes` bytes of `text`.
pub fn char_count(text: &str, bytes: u32) -> u32 {
    let count = text
        .char_indices()
        .take_while(|(offset, _)| *offset < bytes as usize)
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `DeleteSurroundingText` arguments, relative to the cursor and in characters, for
/// deleting `before` and `after` bytes around the cursor. `surrounding` is the text and
/// cursor (in characters) the application reported; without it, a byte is taken for a
/// character.
pub fn delete_surrounding(surrounding: Option<(&str, u32)>, before: u32, after: u32) -> (i32, u32) {
    let (before, after) = match surrounding {
        Some((text, cursor)) => {
            let cursor = byte_offset(text, cursor);
            let start = cursor.saturating_sub(before);
            let end = cursor.saturating_add(after).min(text.len() as u32);
            let start_chars = char_count(text, start);
            let cursor_chars = char_count(text, cursor);
            let end_chars = char_count(text, end);
            (cursor_chars - start_chars, end_chars - cursor_chars)
        }
        None => (before, after),
    };
    (
        -i32::try_from(before).unwrap_or(i32::MAX),
        before.saturating_add(after),
    )
}

/// `UpdateFormattedPreedit` arguments: underlined text and the cursor, in bytes.
pub fn preedit(preedit: &Preedit) -> (Vec<(String, i32)>, i32) {
    if preedit.text.is_empty() {
        return (Vec::new(), 0);
    }
    (
        vec![(preedit.text.clone(), FORMAT_UNDERLINE)],
        preedit.cursor_begin,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_become_content_types() {
        let password = content_type(capability::PASSWORD | capability::SPELL_CHECK);
        assert_eq!(password.purpose, ContentPurpose::Password);
        assert!(password.is_sensitive());
        assert!(content_type(capability::SENSITIVE).is_sensitive());
        let terminal = content_type(capability::TERMINAL | capability::MULTILINE);
        assert_eq!(terminal.purpose, ContentPurpose::Terminal);
        assert_eq!(terminal.hints, ContentHints::MULTILINE);
        assert_eq!(content_type(0), ContentType::default());
        // The bits fcitx 4 reserved and newer ones this frontend ignores change nothing.
        assert_eq!(content_type(1 << 25 | 1 << 39), ContentType::default());
    }

    #[test]
    fn offsets_convert_between_characters_and_bytes() {
        let text = "a中b";
        assert_eq!(byte_offset(text, 0), 0);
        assert_eq!(byte_offset(text, 2), 4);
        assert_eq!(byte_offset(text, 9), 5);
        assert_eq!(char_count(text, 4), 2);
        assert_eq!(char_count(text, 5), 3);
    }

    #[test]
    fn deletions_count_characters() {
        // Cursor after "中" (character 2): delete "中" (3 bytes) and "b" (1 byte).
        assert_eq!(delete_surrounding(Some(("a中b", 2)), 3, 1), (-1, 2));
        assert_eq!(delete_surrounding(Some(("ab", 2)), 9, 0), (-2, 2));
        assert_eq!(delete_surrounding(None, 2, 0), (-2, 2));
    }

    #[test]
    fn preedit_is_underlined_with_a_byte_cursor() {
        let (segments, cursor) = preedit(&Preedit::with_cursor_at_end("中文"));
        assert_eq!(segments, [("中文".to_owned(), FORMAT_UNDERLINE)]);
        assert_eq!(cursor, 6);
        assert_eq!(preedit(&Preedit::default()), (Vec::new(), 0));
    }
}
