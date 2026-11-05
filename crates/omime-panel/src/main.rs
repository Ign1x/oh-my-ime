//! `omime-panel`: oh-my-ime's switcher. When the user switches input methods with
//! omimed's hotkeys or the command line, it shows them for a moment at the bottom of the
//! screen, the one now in use highlighted, as desktops do. omimed starts it with
//! `[panel] enabled`; it learns of switches from the control API's `Switched`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures_util::StreamExt as _;
use omime_overlay::{Overlay, Placement, Pointer};
use slint::{ComponentHandle as _, ModelRc, VecModel};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;
use zbus::OwnedVariant;

slint::slint! {
    export struct Choice {
        label: string,
        name: string,
        current: bool,
    }

    export component Switcher inherits Window {
        in property <[Choice]> choices;
        in property <bool> dark: true;
        // Fades the switcher in and out.
        in property <bool> shown;
        property <length> tile: 88px;
        property <length> gap: 6px;
        property <length> pad: 10px;
        out property <length> panel-width:
            max(1, choices.length) * tile + max(0, choices.length - 1) * gap + 2 * pad;
        out property <length> panel-height: tile + 2 * pad;
        background: transparent;

        Rectangle {
            border-radius: 18px;
            // (Rust cannot take #202124ee in a macro: it reads an exponent.)
            background: root.dark ? rgba(32, 33, 36, 0.93) : #f8f8faf2;
            border-width: 1px;
            border-color: root.dark ? #ffffff1a : #0000001a;
            opacity: root.shown ? 1 : 0;
            animate opacity { duration: 150ms; easing: ease-out; }

            HorizontalLayout {
                padding: root.pad;
                spacing: root.gap;
                for choice in root.choices: Rectangle {
                    width: root.tile;
                    height: root.tile;
                    border-radius: 13px;
                    background: choice.current ? #3478f6 : transparent;

                    VerticalLayout {
                        alignment: center;
                        padding: 6px;
                        spacing: 3px;
                        Text {
                            text: choice.label;
                            font-size: 30px;
                            font-weight: 600;
                            horizontal-alignment: center;
                            color: choice.current ? #ffffff : (root.dark ? #f1f1f3 : #1d1d1f);
                        }
                        Text {
                            text: choice.name;
                            font-size: 12px;
                            horizontal-alignment: center;
                            overflow: elide;
                            color: choice.current ? #ffffffde : (root.dark ? #b9b9be : #6c6c71);
                        }
                    }
                }
            }
        }
    }
}

/// How long the switcher stays after the last switch.
const SHOWN_FOR: Duration = Duration::from_millis(1200);
/// How long it takes to fade out, as the markup animates it, with a little to spare.
const FADE: Duration = Duration::from_millis(200);
/// Its distance from the bottom of the screen, in logical pixels.
const BOTTOM_MARGIN: i32 = 120;

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Daemon1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait Daemon {
    fn input_methods(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;

    #[zbus(signal)]
    fn switched(&self, input_method: &str, reason: &str) -> zbus::Result<()>;
}

/// The desktop's settings, for its color scheme.
#[zbus::proxy(
    interface = "org.freedesktop.portal.Settings",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Settings {
    fn read_one(&self, namespace: &str, key: &str) -> zbus::Result<OwnedVariant>;

    #[zbus(signal)]
    fn setting_changed(
        &self,
        namespace: &str,
        key: &str,
        value: zbus::Variant<'_>,
    ) -> zbus::Result<()>;
}

const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";

/// What the switcher shows: (label, name, whether it is in use) of each choice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choices(Vec<(String, String, bool)>);

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("OMIME_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-panel: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let overlay = omime_overlay::install(
        "omime-switcher",
        Placement {
            bottom: Some(BOTTOM_MARGIN),
            above_fullscreen: true,
            ..Placement::default()
        },
        // The switcher is a notice: clicks go to what is below it.
        Pointer::PassThrough,
    )?;
    let ui = Switcher::new()?;
    ui.show()?;
    VIEW.with(|view| {
        view.borrow_mut().replace(Rc::new(View {
            ui,
            overlay,
            timer: slint::Timer::default(),
            generation: Rc::new(Cell::new(0)),
        }));
    });
    std::thread::Builder::new()
        .name("omime-panel-bus".to_owned())
        .spawn(|| {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(listen()));
            if let Err(e) = result {
                warn!("{e:#}");
            }
            // Without omimed's signals there is nothing to show.
            let _ = slint::quit_event_loop();
        })?;
    slint::run_event_loop()?;
    Ok(())
}

thread_local! {
    static VIEW: RefCell<Option<Rc<View>>> = const { RefCell::new(None) };
}

/// The switcher on the screen, on the thread of the user interface.
struct View {
    ui: Switcher,
    overlay: Overlay,
    /// Fades the switcher out after the last switch.
    timer: slint::Timer,
    /// Counts the times it was shown, so that a fade ends only the showing it began in.
    generation: Rc<Cell<u64>>,
}

impl View {
    fn show(&self, choices: Choices, dark: bool) {
        let model: Vec<Choice> = choices
            .0
            .into_iter()
            .map(|(label, name, current)| Choice {
                label: label.into(),
                name: name.into(),
                current,
            })
            .collect();
        self.ui.set_choices(ModelRc::new(VecModel::from(model)));
        self.ui.set_dark(dark);
        let width = self.ui.get_panel_width().ceil() as u32;
        let height = self.ui.get_panel_height().ceil() as u32;
        self.overlay.show(width, height);
        self.ui.set_shown(true);

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ui = self.ui.as_weak();
        let overlay = self.overlay.clone();
        let latest = self.generation.clone();
        self.timer
            .start(slint::TimerMode::SingleShot, SHOWN_FOR, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.set_shown(false);
                }
                let overlay = overlay.clone();
                let latest = latest.clone();
                slint::Timer::single_shot(FADE, move || {
                    if latest.get() == generation {
                        overlay.hide();
                    }
                });
            });
    }
}

/// Follows omimed's switches, and the desktop's color scheme.
async fn listen() -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let daemon = DaemonProxy::new(&conn).await?;
    let mut switches = daemon.receive_switched().await?;
    let settings = SettingsProxy::new(&conn).await.ok();
    let mut dark = match &settings {
        Some(settings) => read_dark(settings).await,
        None => true,
    };
    let mut scheme_changes = match &settings {
        Some(settings) => settings
            .receive_setting_changed_with_args(&[(0, APPEARANCE), (1, COLOR_SCHEME)])
            .await
            .ok(),
        None => None,
    };
    info!("waiting for switches");
    loop {
        tokio::select! {
            switch = switches.next() => {
                let Some(switch) = switch else { break };
                let Ok(args) = switch.args() else { continue };
                let (input_method, reason) = (*args.input_method(), *args.reason());
                // Menus show what they switch to; applications get theirs back quietly.
                if !matches!(reason, "hotkey" | "command") {
                    debug!(input_method, reason, "not showing the switcher");
                    continue;
                }
                let methods = daemon.input_methods().await.unwrap_or_default();
                let choices = choices(&methods, input_method);
                debug!(input_method, reason, "showing the switcher");
                slint::invoke_from_event_loop(move || {
                    VIEW.with(|view| {
                        if let Some(view) = view.borrow().clone() {
                            view.show(choices, dark);
                        }
                    });
                })?;
            }
            change = async {
                match &mut scheme_changes {
                    Some(changes) => changes.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match change {
                    Some(change) => {
                        if let Ok(args) = change.args() {
                            dark = is_dark(args.value());
                        }
                    }
                    None => scheme_changes = None,
                }
            }
        }
    }
    Ok(())
}

/// Whether the desktop prefers dark colors; without a preference the switcher is dark.
async fn read_dark(settings: &SettingsProxy<'_>) -> bool {
    match settings.read_one(APPEARANCE, COLOR_SCHEME).await {
        Ok(value) => is_dark(&value),
        Err(e) => {
            debug!("no color scheme from the desktop: {e}");
            true
        }
    }
}

/// The portal's color scheme: 1 prefers dark, 2 prefers light, 0 has no preference.
fn is_dark(value: &zbus::Variant<'_>) -> bool {
    let value = match value {
        // Older portals wrap the value once more.
        zbus::Variant::Variant(inner) => inner.as_ref(),
        other => other,
    };
    !matches!(value, zbus::Variant::U32(2))
}

/// What the switcher shows: direct input, then the enabled input methods in switching
/// order (`InputMethods` of the control API), with `current` (empty: direct input) in
/// use.
fn choices(methods: &[HashMap<String, OwnedVariant>], current: &str) -> Choices {
    let text = |method: &HashMap<String, OwnedVariant>, key: &str| {
        method
            .get(key)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    let enabled = |method: &&HashMap<String, OwnedVariant>| {
        method
            .get("enabled")
            .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
            .unwrap_or(false)
    };
    let mut choices = vec![(
        "A".to_owned(),
        direct_input_label().to_owned(),
        current.is_empty(),
    )];
    for method in methods.iter().filter(enabled) {
        let id = text(method, "id");
        let name = text(method, "name");
        let mut label = text(method, "label");
        if label.is_empty() {
            label = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
        }
        choices.push((label, name, !current.is_empty() && id == current));
    }
    Choices(choices)
}

/// The direct-input entry in the switcher.
fn direct_input_label() -> &'static str {
    omime_proto::i18n::text("Direct input")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(id: &str, name: &str, label: &str, enabled: bool) -> HashMap<String, OwnedVariant> {
        let owned = |v: zbus::Variant<'_>| OwnedVariant::try_from(v).expect("no file descriptors");
        HashMap::from([
            ("id".to_owned(), owned(zbus::Variant::from(id))),
            ("name".to_owned(), owned(zbus::Variant::from(name))),
            ("label".to_owned(), owned(zbus::Variant::from(label))),
            ("enabled".to_owned(), owned(zbus::Variant::from(enabled))),
        ])
    }

    #[test]
    fn the_switcher_shows_direct_input_and_the_enabled_input_methods() {
        let methods = [
            method("fcitx5/shuangpin", "双拼", "双", true),
            method("sparkle", "Sparkle", "", true),
            method("fcitx5/rime", "Rime", "", false),
        ];
        let Choices(listed) = choices(&methods, "fcitx5/shuangpin");
        let shown: Vec<(&str, &str, bool)> = listed
            .iter()
            .map(|(label, name, current)| (label.as_str(), name.as_str(), *current))
            .collect();
        assert_eq!(shown[1..], [("双", "双拼", true), ("S", "Sparkle", false)]);
        assert_eq!((shown[0].0, shown[0].2), ("A", false));
        assert!(choices(&methods, "").0[0].2, "direct input is in use");
    }

    #[test]
    fn color_schemes_are_dark_unless_light() {
        assert!(is_dark(&zbus::Variant::U32(1)));
        assert!(is_dark(&zbus::Variant::U32(0)));
        assert!(!is_dark(&zbus::Variant::U32(2)));
        let wrapped = zbus::Variant::Variant(Box::new(zbus::Variant::U32(2)));
        assert!(!is_dark(&wrapped));
    }
}
