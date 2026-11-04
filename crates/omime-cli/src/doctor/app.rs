//! Read-only diagnostics for a selected process. Never read command arguments,
//! window titles, document contents, or print unrelated environment variables.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use anyhow::{Context as _, ensure};

use super::{Finding, environment_findings};

pub const VARIABLES: [&str; 7] = [
    "QT_IM_MODULE",
    "QT_IM_MODULES",
    "GTK_IM_MODULE",
    "XMODIFIERS",
    "QT_QPA_PLATFORM",
    "DISPLAY",
    "WAYLAND_DISPLAY",
];

#[derive(Debug)]
pub struct Application {
    pub pid: u32,
    name: String,
    pub variables: Vec<(String, String)>,
    plugins: BTreeSet<&'static str>,
    maps_readable: bool,
}

impl Application {
    pub fn read(pid: u32) -> anyhow::Result<Self> {
        let path = Path::new("/proc").join(pid.to_string());
        let uid = std::fs::metadata("/proc/self")?.uid();
        ensure!(
            std::fs::metadata(&path)
                .with_context(|| format!(
                    "application {pid} is no longer running or cannot be inspected"
                ))?
                .uid()
                == uid,
            "application {pid} belongs to another user"
        );
        let name = std::fs::read_to_string(path.join("comm"))?
            .trim()
            .to_owned();
        let environment = std::fs::read(path.join("environ"))
            .with_context(|| format!("cannot read application {pid}'s launch environment"))?;
        let maps = std::fs::read_to_string(path.join("maps"));
        Ok(Self {
            pid,
            name,
            variables: input_variables(&environment),
            plugins: loaded_plugins(maps.as_deref().unwrap_or("")),
            maps_readable: maps.is_ok(),
        })
    }

    pub fn findings(&self) -> Vec<Finding> {
        let mut findings = vec![Finding::ok(format!(
            "application {} ({}): launch environment",
            self.pid, self.name
        ))];
        for key in VARIABLES {
            let value = self
                .variables
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v);
            let value = value.map_or_else(|| "(unset)".to_owned(), |s| format!("{s:?}"));
            findings.push(Finding::ok(format!("{key}={value}")));
        }
        findings.extend(environment_findings(&self.variables));
        for plugin in &self.plugins {
            let message = format!("loaded input backend: {plugin}");
            findings.push(if *plugin == "Qt Compose" {
                Finding::warning(format!(
                    "{message}; Compose does not connect to the platform's Chinese input methods"
                ))
            } else {
                Finding::ok(message)
            });
        }
        if self.plugins.is_empty() {
            findings.push(Finding::warning(if self.maps_readable {
                "no known input plugin is visible in this process's loaded libraries; static plugins and toolkit defaults may still work"
            } else {
                "loaded libraries cannot be inspected; input plugin detection is unavailable"
            }));
        }
        findings.push(Finding::ok("This check reads launch settings and loaded libraries. It does not verify key delivery."));
        findings
    }
}

fn input_variables(bytes: &[u8]) -> Vec<(String, String)> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let split = entry.iter().position(|b| *b == b'=')?;
            let key = std::str::from_utf8(&entry[..split]).ok()?;
            VARIABLES.contains(&key).then(|| {
                (
                    key.to_owned(),
                    String::from_utf8_lossy(&entry[split + 1..]).into_owned(),
                )
            })
        })
        .collect()
}

fn loaded_plugins(maps: &str) -> BTreeSet<&'static str> {
    let mut plugins = BTreeSet::new();
    for line in maps.lines() {
        for (needle, label) in [
            ("libfcitxplatforminputcontextplugin.so", "Qt Fcitx"),
            ("libfcitx5platforminputcontextplugin.so", "Qt Fcitx"),
            ("libibusplatforminputcontextplugin.so", "Qt IBus"),
            ("libcomposeplatforminputcontextplugin.so", "Qt Compose"),
            ("/im-fcitx.so", "GTK Fcitx"),
            ("/im-fcitx5.so", "GTK Fcitx"),
            ("/im-ibus.so", "GTK IBus"),
            ("/im-xim.so", "GTK XIM"),
            ("/platforms/libqwayland", "Qt Wayland (display backend)"),
            ("/platforms/libqxcb.so", "Qt X11 (display backend)"),
        ] {
            if line.contains(needle) {
                plugins.insert(label);
            }
        }
    }
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_input_environment_is_reported() {
        let variables = input_variables(
            b"PRIVATE_TOKEN=do-not-print\0QT_IM_MODULE=wayland\0XMODIFIERS=@im=fcitx\0OTHER=a=b\0",
        );
        assert_eq!(
            variables,
            [
                ("QT_IM_MODULE".into(), "wayland".into()),
                ("XMODIFIERS".into(), "@im=fcitx".into())
            ]
        );
    }

    #[test]
    fn compose_fallback_is_distinct_from_a_wayland_display_backend() {
        let app = Application {
            pid: 123,
            name: "wps".into(),
            variables: Vec::new(),
            maps_readable: true,
            plugins: loaded_plugins(
                "000 r-xp /qt/plugins/platforminputcontexts/libcomposeplatforminputcontextplugin.so\n000 r-xp /qt/plugins/platforms/libqxcb.so\n",
            ),
        };
        let findings = app.findings();
        assert!(
            findings
                .iter()
                .any(|f| f.level == super::super::Level::Warning && f.text.contains("Qt Compose"))
        );
        assert!(app.plugins.contains("Qt X11 (display backend)"));
        assert!(!app.plugins.contains("Qt Fcitx"));
    }
}
