//! IBus's documented serializable values. Offsets on its wire are Unicode characters.
use omime_proto::engine::CandidatePage;
use std::collections::HashMap;
use zbus::{OwnedVariant, Variant};

fn own(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("IBus values contain no descriptors")
}
fn attachments() -> HashMap<String, OwnedVariant> {
    HashMap::new()
}
pub fn text(value: &str) -> OwnedVariant {
    let attrs = own(Variant::from((
        "IBusAttrList",
        attachments(),
        Vec::<OwnedVariant>::new(),
    )));
    own(Variant::from(("IBusText", attachments(), value, attrs)))
}
pub fn read_text(value: &OwnedVariant) -> Option<String> {
    let structure: zbus::Structure<'_> = value.try_clone().ok()?.try_into().ok()?;
    if <&str>::try_from(structure.fields().first()?).ok()? != "IBusText" {
        return None;
    }
    <&str>::try_from(structure.fields().get(2)?)
        .ok()
        .map(str::to_owned)
}
pub fn lookup(page: &CandidatePage) -> OwnedVariant {
    let mut candidates: Vec<_> = page
        .candidates
        .iter()
        .map(|c| {
            text(&if c.comment.is_empty() {
                c.text.clone()
            } else {
                format!("{}  {}", c.text, c.comment)
            })
        })
        .collect();
    let labels: Vec<_> = page
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            text(&if c.label.is_empty() {
                (i + 1).to_string()
            } else {
                c.label.clone()
            })
        })
        .collect();
    let page_size = (candidates.len() as u32).clamp(1, 16);
    let prefix = if page.has_previous { page_size } else { 0 };
    if page.has_previous {
        let mut previous = (0..page_size).map(|_| text("")).collect::<Vec<_>>();
        previous.append(&mut candidates);
        candidates = previous;
    }
    if page.has_next {
        candidates.extend((0..page_size).map(|_| text("")));
    }
    own(Variant::from((
        "IBusLookupTable",
        attachments(),
        page_size,
        prefix + page.cursor.min(page_size - 1),
        true,
        false,
        1i32,
        candidates,
        labels,
    )))
}
pub fn component() -> OwnedVariant {
    // EngineDesc fields are append-only. The stable prefix through setup is accepted
    // by all supported IBus releases.
    let engine = own(Variant::from((
        "IBusEngineDesc",
        attachments(),
        "oh-my-ime",
        "oh-my-ime",
        "oh-my-ime input methods",
        "und",
        "MIT",
        "oh-my-ime contributors",
        "input-keyboard",
        "default",
        0u32,
        "",
        "IM",
        "omime-settings",
    )));
    own(Variant::from((
        "IBusComponent",
        attachments(),
        "org.freedesktop.IBus.OhMyIme",
        "oh-my-ime input methods",
        env!("CARGO_PKG_VERSION"),
        "MIT",
        "oh-my-ime contributors",
        "https://github.com/ign1x/oh-my-ime",
        "omimed --ibus",
        "oh-my-ime",
        Vec::<OwnedVariant>::new(),
        vec![engine],
    )))
}
pub fn byte_offset(text: &str, chars: u32) -> usize {
    text.char_indices()
        .nth(chars as usize)
        .map_or(text.len(), |(i, _)| i)
}
pub fn char_offset(text: &str, bytes: usize) -> u32 {
    let mut bytes = bytes.min(text.len());
    while !text.is_char_boundary(bytes) {
        bytes -= 1;
    }
    text[..bytes].chars().count() as u32
}

/// Bound retained surrounding text before queue admission, preserving character
/// offsets for IBus. The actor independently enforces its byte-offset contract.
pub fn surrounding_window(text: &str, cursor: u32, anchor: u32) -> (String, u32, u32) {
    let cursor_byte = byte_offset(text, cursor);
    let mut start = cursor_byte
        .saturating_sub(2000)
        .min(text.len().saturating_sub(4000));
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + 4000).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let offset = char_offset(text, start);
    let window = text[start..end].to_owned();
    let count = window.chars().count() as u32;
    (
        window,
        cursor.saturating_sub(offset).min(count),
        anchor.saturating_sub(offset).min(count),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_offsets_and_text_roundtrip() {
        let value = "a你🙂z";
        assert_eq!(byte_offset(value, 3), 8);
        assert_eq!(char_offset(value, 7), 2);
        assert_eq!(read_text(&text(value)).as_deref(), Some(value));
    }
}
