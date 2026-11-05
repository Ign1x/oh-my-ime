//! Managed configuration edits and an on-disk journal for setup/undo.

use omime_proto::tr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edit {
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Kind {
    Whole,
    Text { edits: Vec<Edit> },
    Config { keys: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedFile {
    pub path: PathBuf,
    pub original: Option<String>,
    /// Actual contents before this application, also retained during an interrupted update.
    pub previous: Option<String>,
    pub installed: String,
    pub mode: u32,
    pub change: Kind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub complete: bool,
    pub config_path: PathBuf,
    pub engine: Option<String>,
    pub take_over: bool,
    pub files: Vec<ManagedFile>,
}

impl Record {
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        let Some(text) = read(path)? else {
            return Ok(None);
        };
        let record: Self = toml::from_str(&text)
            .with_context(|| tr!("cannot read setup journal {arg0}", arg0 = path.display()))?;
        ensure!(
            record.version == 1,
            "{}",
            tr!(
                "unsupported setup journal version {arg0}; keep {arg1} for recovery",
                arg0 = record.version,
                arg1 = path.display()
            )
        );
        Ok(Some(record))
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        atomic_write(path, &toml::to_string(self)?, 0o600)
    }
}

/// Reads ordinary files only. Replacing a dotfile-manager symlink would silently
/// detach the user's declarative configuration from its source.
pub fn read(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                "{}",
                tr!(
                    "{arg0} is a symlink managed outside setup; use a writable --config path or manage the include in your dotfile configuration",
                    arg0 = path.display()
                )
            );
            ensure!(
                meta.is_file(),
                "{}",
                tr!("{arg0} is not a regular file", arg0 = path.display())
            );
            Ok(Some(fs::read_to_string(path).with_context(|| {
                tr!("cannot read {arg0}", arg0 = path.display())
            })?))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| tr!("cannot inspect {arg0}", arg0 = path.display())),
    }
}

pub fn mode(path: &Path) -> u32 {
    fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777)
}

/// Removes only managed edits, carrying unrelated user changes forward.
pub fn unmanage(file: &ManagedFile, current: Option<&str>) -> anyhow::Result<Option<String>> {
    if current == file.original.as_deref() {
        return Ok(file.original.clone()); // Also covers an interrupted, unwritten edit.
    }
    if current == Some(file.installed.as_str()) {
        return Ok(file.original.clone());
    }
    let conflict = || {
        anyhow::anyhow!(
            "{}",
            tr!(
                "{path} changed in a section managed by setup; keep the journal and reconcile that section before --undo",
                path = file.path.display()
            )
        )
    };
    let current = current.ok_or_else(conflict)?;
    match &file.change {
        Kind::Whole => Err(conflict()),
        Kind::Text { edits } => {
            let mut text = current.to_owned();
            for edit in edits.iter().rev() {
                ensure!(text.matches(&edit.after).count() == 1, "{}", conflict());
                let at = text.find(&edit.after).ok_or_else(conflict)?;
                // The appended block may have supplied a missing final newline.
                // Keep that separator if the user later appended a new directive.
                let separator = edit.before.is_empty()
                    && edit.after.starts_with('\n')
                    && at + edit.after.len() < text.len();
                let before = if separator { "\n" } else { &edit.before };
                text = text.replacen(&edit.after, before, 1);
            }
            Ok(Some(text))
        }
        Kind::Config { keys } => {
            let mut doc: toml_edit::DocumentMut = current.parse()?;
            let original: toml_edit::DocumentMut =
                file.original.as_deref().unwrap_or("").parse()?;
            let current_values: toml::Value = toml::from_str(current)?;
            let installed_values: toml::Value = toml::from_str(&file.installed)?;
            for key in keys {
                // Compare values, not whitespace or comments the user may have edited.
                ensure!(
                    current_values.get(key) == installed_values.get(key),
                    "{}",
                    conflict()
                );
                match original.get(key) {
                    Some(item) => {
                        doc[key] = item.clone();
                    }
                    None => {
                        doc.remove(key);
                    }
                }
            }
            Ok(Some(doc.to_string()))
        }
    }
}

#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    pub mode: u32,
}

impl Change {
    fn apply(&self) -> anyhow::Result<()> {
        ensure!(
            read(&self.path)? == self.before,
            "{}",
            tr!(
                "{arg0} changed while setup was preparing; retry after reviewing it",
                arg0 = self.path.display()
            )
        );
        replace(&self.path, self.after.as_deref(), self.mode)
    }

    fn rollback(&self) -> anyhow::Result<()> {
        ensure!(
            read(&self.path)? == self.after,
            "{}",
            tr!(
                "{arg0} changed during rollback; recover it using the setup journal",
                arg0 = self.path.display()
            )
        );
        replace(&self.path, self.before.as_deref(), self.mode)
    }
}

pub fn apply(
    journal: &Path,
    old: Option<&Record>,
    mut record: Record,
    changes: &[Change],
) -> anyhow::Result<()> {
    record.complete = false;
    // Save originals before the first mutation, including on process interruption.
    record.save(journal)?;
    for (done, change) in changes.iter().enumerate() {
        if let Err(error) = change.apply() {
            let mut rollback_errors = Vec::new();
            for previous in changes[..done].iter().rev() {
                if let Err(e) = previous.rollback() {
                    rollback_errors.push(e.to_string());
                }
            }
            if rollback_errors.is_empty() {
                match old {
                    Some(old) => old.save(journal)?,
                    None => fs::remove_file(journal)?,
                }
                return Err(error);
            }
            bail!(
                "{}",
                tr!(
                    "{error_0}; rollback needs attention: {arg0}. Journal: {arg1}",
                    error_0 = format!("{:#}", error),
                    arg0 = rollback_errors.join("; "),
                    arg1 = journal.display()
                )
            );
        }
    }
    record.complete = true;
    record.save(journal)
}

pub fn undo(journal: &Path, changes: &[Change]) -> anyhow::Result<()> {
    for (done, change) in changes.iter().enumerate() {
        if let Err(error) = change.apply() {
            for previous in changes[..done].iter().rev() {
                previous.rollback()?;
            }
            return Err(error);
        }
    }
    fs::remove_file(journal).with_context(|| tr!("cannot remove {arg0}", arg0 = journal.display()))
}

fn replace(path: &Path, text: Option<&str>, mode: u32) -> anyhow::Result<()> {
    match text {
        Some(text) => atomic_write(path, text, mode),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| tr!("cannot remove {arg0}", arg0 = path.display())),
        },
    }
}

fn atomic_write(path: &Path, text: &str, mode: u32) -> anyhow::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .context(tr!("configuration path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| tr!("cannot create {arg0}", arg0 = parent.display()))?;
    let temp = parent.join(format!(
        ".omime-setup-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        // The target may not have existed when the plan was made. Do not replace a
        // symlink that appeared since; all managed files are regular files.
        let _ = read(path)?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| tr!("cannot write {arg0}", arg0 = path.display()))
}
