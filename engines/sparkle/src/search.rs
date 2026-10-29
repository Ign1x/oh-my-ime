//! Finding emoji by shortcode or name.

/// One emoji the user can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub glyph: &'static str,
    /// The shortcode that matched best, shown under the glyph.
    pub label: &'static str,
}

struct Entry {
    glyph: &'static str,
    shortcodes: Vec<&'static str>,
    /// Lower-case CLDR name, e.g. "grinning face with big eyes".
    name: String,
}

/// All emoji (default skin tone), searchable.
pub struct Index {
    entries: Vec<Entry>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Index {
    pub fn new() -> Self {
        let entries = emojis::iter()
            .filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
            .filter_map(|e| {
                let shortcodes: Vec<&'static str> = e.shortcodes().collect();
                (!shortcodes.is_empty()).then(|| Entry {
                    glyph: e.as_str(),
                    shortcodes,
                    name: e.name().to_lowercase(),
                })
            })
            .collect();
        Self { entries }
    }

    /// The best `limit` matches for `query`: exact shortcodes first, then shortcode
    /// prefixes, name word prefixes and finally substrings.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Candidate> {
        let query = query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(u8, usize, usize, Candidate)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(order, entry)| {
                let (rank, label) = rank(entry, &query)?;
                Some((
                    rank,
                    label.len(),
                    order,
                    Candidate {
                        glyph: entry.glyph,
                        label,
                    },
                ))
            })
            .collect();
        scored.sort_by_key(|(rank, len, order, _)| (*rank, *len, *order));
        scored.into_iter().take(limit).map(|(.., c)| c).collect()
    }
}

fn rank(entry: &Entry, query: &str) -> Option<(u8, &'static str)> {
    let shortcodes = || entry.shortcodes.iter().copied();
    if let Some(code) = shortcodes().find(|c| *c == query) {
        return Some((0, code));
    }
    if let Some(code) = shortcodes().find(|c| c.starts_with(query)) {
        return Some((1, code));
    }
    let first = entry.shortcodes[0];
    if entry.name.split([' ', '-']).any(|w| w.starts_with(query)) {
        return Some((2, first));
    }
    if let Some(code) = shortcodes().find(|c| c.contains(query)) {
        return Some((3, code));
    }
    entry.name.contains(query).then_some((4, first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_shortcodes_come_first() {
        let index = Index::new();
        let results = index.search("smile", 8);
        assert_eq!(results[0].glyph, "😄");
        assert_eq!(results[0].label, "smile");
        assert!(results.len() <= 8);
    }

    #[test]
    fn prefixes_and_names_match() {
        let index = Index::new();
        assert!(index.search("thumb", 8).iter().any(|c| c.glyph == "👍"));
        assert!(index.search("ROCKET", 8).iter().any(|c| c.glyph == "🚀"));
        assert!(index.search("", 8).is_empty());
        assert!(index.search("zzzzzzzz", 8).is_empty());
    }

    #[test]
    fn skin_tone_variants_are_not_listed() {
        let index = Index::new();
        let waves: Vec<_> = index
            .search("wave", 50)
            .into_iter()
            .filter(|c| c.glyph.starts_with('👋'))
            .collect();
        assert_eq!(waves.len(), 1);
    }
}
