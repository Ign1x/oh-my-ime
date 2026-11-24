//! Protocol capabilities shared by startup and the non-invasive setup probe.
//!
//! Desktop names cannot establish support: nested compositors and inherited sockets
//! may belong to another desktop. Only the registry of the actual connection does.

/// The protocols implemented by the current Wayland frontend.
pub const REQUIRED: [&str; 2] = [
    "zwp_input_method_manager_v2",
    "zwp_virtual_keyboard_manager_v1",
];

/// A snapshot of the input protocols advertised on one Wayland connection.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub input_method_v2: bool,
    pub virtual_keyboard_v1: bool,
    pub input_method_v1: bool,
    pub input_panel_v1: bool,
}

impl Capabilities {
    pub fn from_globals<'a>(globals: impl IntoIterator<Item = (&'a str, u32)>) -> Self {
        let mut capabilities = Self::default();
        for (interface, version) in globals {
            if version == 0 {
                continue;
            }
            match interface {
                "zwp_input_method_manager_v2" => capabilities.input_method_v2 = true,
                "zwp_virtual_keyboard_manager_v1" => capabilities.virtual_keyboard_v1 = true,
                "zwp_input_method_v1" => capabilities.input_method_v1 = true,
                "zwp_input_panel_v1" => capabilities.input_panel_v1 = true,
                _ => {}
            }
        }
        capabilities
    }

    pub fn supported(self) -> bool {
        (self.input_method_v2 && self.virtual_keyboard_v1)
            || (self.input_method_v1 && self.input_panel_v1)
    }

    pub fn missing(self) -> Vec<&'static str> {
        if self.supported() {
            return Vec::new();
        }
        [self.input_method_v2, self.virtual_keyboard_v1]
            .into_iter()
            .zip(REQUIRED)
            .filter_map(|(present, name)| (!present).then_some(name))
            .collect()
    }

    /// Actionable wording for a connection without a complete input route.
    pub fn warning(self) -> Option<String> {
        if self.supported() {
            return None;
        }
        let missing = self.missing().join(", ");
        Some(if self.input_method_v1 {
            crate::tr!(
                "This desktop is not supported: missing {protocols}. This connection provides input-method-v1 but no input-panel-v1. Select oh-my-ime in KDE Virtual Keyboard settings so KWin supplies its privileged input connection.",
                protocols = missing
            )
        } else {
            crate::tr!(
                "This desktop is not supported: missing {protocols}. On KDE Plasma select oh-my-ime in Virtual Keyboard settings; on GNOME use omimed --ibus with the system IBus service. A Hyprland session or sway uses input-method-v2.",
                protocols = missing
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_support_requires_both_live_v2_protocols() {
        assert_eq!(Capabilities::default().missing(), REQUIRED);
        for (present, missing) in [(0, 1), (1, 0)] {
            let caps = Capabilities::from_globals([(REQUIRED[present], 1)]);
            assert!(!caps.supported());
            assert_eq!(caps.missing(), [REQUIRED[missing]]);
        }
        let caps = Capabilities::from_globals([
            (REQUIRED[0], 2),
            (REQUIRED[1], 1),
            ("zwp_input_method_v1", 1),
        ]);
        assert!(caps.supported());
        assert!(caps.warning().is_none());
        assert!(!Capabilities::from_globals([(REQUIRED[0], 0), (REQUIRED[1], 1)]).supported());
    }

    #[test]
    fn privileged_v1_requires_both_text_and_panel_protocols() {
        let caps = Capabilities::from_globals([
            ("zwp_input_method_v1", 1),
            ("zwp_input_panel_v1", 1),
            ("wl_compositor", 6),
        ]);
        assert!(caps.supported());
        assert!(caps.warning().is_none());
        assert!(caps.missing().is_empty());
        let incomplete = Capabilities::from_globals([("zwp_input_method_v1", 1)]);
        assert!(!incomplete.supported());
        assert!(incomplete.warning().unwrap().contains("input-panel-v1"));
    }
}
