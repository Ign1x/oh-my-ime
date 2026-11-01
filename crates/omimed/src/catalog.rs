//! What omimed knows about the input methods of hosted frameworks, kept across restarts
//! in `$XDG_CACHE_HOME/oh-my-ime/catalog.toml`, so that every input method of a
//! framework can be listed before the framework runs again. A cache: losing it costs a
//! framework start, nothing more. Unlike the state, it changes only when a framework
//! reports something new.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::core::frameworks::InnerMethod;

const CATALOG_FILE: &str = "catalog.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    /// Every input method of each framework, set up or not, by engine.
    pub available: BTreeMap<String, Vec<InnerMethod>>,
}

fn path() -> Option<PathBuf> {
    omime_proto::paths::cache_dir().map(|d| d.join(CATALOG_FILE))
}

impl Catalog {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                warn!("ignoring {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the catalog atomically (write to a temporary file, then rename).
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text = toml::to_string(self).map_err(std::io::Error::other)?;
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            warn!("cannot save {}: {e}", path.display());
        }
    }
}
