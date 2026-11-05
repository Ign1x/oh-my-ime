//! Shared vocabulary of the oh-my-ime platform.
//!
//! This crate is used by the daemon (`omimed`), the CLI and the engine SDK. It defines
//! names, versions, wire types, manifests and configuration so that every component
//! agrees on them. Apart from reading manifest directories it performs no I/O.

pub mod config;
pub mod content;
pub mod engine;
pub mod i18n;
pub mod manifest;
pub mod names;
pub mod paths;

pub use content::{ContentHints, ContentPurpose, ContentType};

/// Version of the engine protocol (`io.github.ign1x.OhMyIme.Engine1` and friends).
///
/// The major version is part of the D-Bus interface names. The minor version is
/// negotiated when an engine registers; additions within a major version must be
/// backwards compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl ApiVersion {
    /// The engine protocol version implemented by this crate.
    pub const CURRENT: Self = Self { major: 1, minor: 1 };

    /// Returns whether a peer speaking `other` can talk to us.
    pub fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Reads `T` from TOML, leaving out keys `T` does not know, and returns them as dotted
/// paths (`hotkeys.togle`), sorted. Files written for a newer version must still load
/// in older ones, so unknown keys are for warnings, not errors.
pub(crate) fn from_toml<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<(T, Vec<String>), String> {
    let mut unknown = Vec::new();
    let deserializer = toml::Deserializer::parse(text).map_err(|e| e.to_string())?;
    let value = serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()))
        .map_err(|e| e.to_string())?;
    unknown.sort();
    Ok((value, unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_toml_keys_are_reported_not_rejected() {
        #[derive(Debug, serde::Deserialize)]
        struct Inner {
            known: bool,
        }
        #[derive(Debug, serde::Deserialize)]
        struct Outer {
            inner: Inner,
        }
        let (value, unknown) =
            from_toml::<Outer>("extra = 1\n[inner]\nknown = true\nnew = \"x\"\n").unwrap();
        assert!(value.inner.known);
        assert_eq!(unknown, ["extra", "inner.new"]);
        // Wrong types and syntax are still errors.
        assert!(from_toml::<Outer>("[inner]\nknown = 1\n").is_err());
        assert!(from_toml::<Outer>("[inner\n").is_err());
    }

    #[test]
    fn api_compatibility_follows_major_version() {
        let current = ApiVersion::CURRENT;
        assert!(current.is_compatible_with(ApiVersion { major: 1, minor: 7 }));
        assert!(!current.is_compatible_with(ApiVersion { major: 2, minor: 0 }));
        assert_eq!(current.to_string(), "1.1");
    }
}
