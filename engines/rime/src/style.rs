//! How the candidate window looks: `$XDG_CONFIG_HOME/oh-my-ime/rime.toml`, which the
//! settings window writes and the engine reads when it starts.
//!
//! ```toml
//! # Candidates in a row ("horizontal") or a column ("vertical").
//! layout = "horizontal"
//! # "system" follows the desktop; "light" or "dark".
//! appearance = "system"
//! # The size of the candidates, in logical pixels (12 to 32).
//! font_size = 18
//! ```

use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Horizontal,
    Vertical,
}

impl Layout {
    /// The choices, in the order of the settings window.
    pub const ALL: [Self; 2] = [Self::Horizontal, Self::Vertical];

    /// The name in the file.
    fn name(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// As the desktop prefers.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// The choices, in the order of the settings window.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub layout: Layout,
    pub appearance: Appearance,
    pub font_size: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            appearance: Appearance::default(),
            font_size: 18,
        }
    }
}

/// The font sizes the window takes.
pub const FONT_SIZES: std::ops::RangeInclusive<u32> = 12..=32;

impl Style {
    /// `$XDG_CONFIG_HOME/oh-my-ime/rime.toml`.
    pub fn path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))?;
        Some(config.join("oh-my-ime/rime.toml"))
    }

    /// The style in the file, or the default when there is none or it has a mistake,
    /// which is reported.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                eprintln!("omime-rime: {}: {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("omime-rime: cannot read {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut style: Self = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        style.font_size = style
            .font_size
            .clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
        Ok(style)
    }

    /// The file's text, with what each key means.
    pub fn text(&self) -> String {
        format!(
            "# The candidate window of oh-my-ime's Rime engine, which reads this file when it\n\
             # starts. Its settings window (`omime settings rime`) writes it.\n\
             \n\
             # Candidates in a row (\"horizontal\") or a column (\"vertical\").\n\
             layout = \"{}\"\n\
             # \"system\" follows the desktop; \"light\" or \"dark\".\n\
             appearance = \"{}\"\n\
             # The size of the candidates, in logical pixels ({} to {}).\n\
             font_size = {}\n",
            self.layout.name(),
            self.appearance.name(),
            FONT_SIZES.start(),
            FONT_SIZES.end(),
            self.font_size,
        )
    }

    /// Writes the file, through a temporary file that replaces it.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no home directory"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("no settings directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
        temporary.write_all(self.text().as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_reads_back_and_fills_in_what_it_lacks() {
        let style = Style {
            layout: Layout::Vertical,
            appearance: Appearance::Dark,
            font_size: 22,
        };
        assert_eq!(Style::parse(&style.text()), Ok(style));
        assert_eq!(
            Style::parse("layout = \"vertical\"\nunknown = 1\n"),
            Ok(Style {
                layout: Layout::Vertical,
                ..Style::default()
            })
        );
        assert_eq!(Style::parse("").map(|s| s.font_size), Ok(18));
        assert_eq!(Style::parse("font_size = 99").map(|s| s.font_size), Ok(32));
        assert!(Style::parse("layout = \"diagonal\"").is_err());
    }
}
