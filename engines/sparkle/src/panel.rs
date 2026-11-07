//! The recent-emoji panel: an optional floating panel at the bottom right of the screen
//! with the emoji inserted last, which a click inserts again into the text field the
//! user types in. Sparkle's menu turns it on and off.
//!
//! It is a process of its own, `omime-sparkle --panel`. Sparkle's deck is a popup on
//! omimed's UI bridge, while a panel is an ordinary layer surface of the compositor in
//! `$OMIME_HOST_WAYLAND_DISPLAY`, drawn with `omime-overlay`; one process cannot be a
//! Slint application on both. The panel takes the pointer but not the keyboard, which
//! stays in the text field. The engine talks to it over its standard input and output,
//! one line each:
//!
//! - engine → panel: `show`, `hide`, `recent 🚀 ✨ …`
//! - panel → engine: `pick 🚀`
//!
//! The panel ends when its standard input does, with the engine.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;

use omime_overlay::{Overlay, Placement, Pointer};
use slint::{ComponentHandle as _, Model as _, ModelRc, VecModel};

use crate::emoji::Painter;

slint::slint! {
    export struct Tile {
        glyph: string,
        // The emoji in color, when there is an emoji font (see emoji.rs).
        picture: image,
        painted: bool,
    }

    export component RecentPanel inherits Window {
        in property <[Tile]> tiles;
        callback picked(int);

        property <length> tile: 44px;
        property <length> pad: 8px;
        out property <length> panel-width: max(1, root.tiles.length) * root.tile + 2 * root.pad;
        out property <length> panel-height: root.tile + 2 * root.pad;
        background: transparent;

        Rectangle {
            border-radius: 16px;
            // Sparkle's colours, as its deck, but plain: the software renderer fills a
            // gradient into the rounded corners too.
            background: #2c2468f5;
            border-width: 1px;
            border-color: #ffffff2e;

            HorizontalLayout {
                padding: root.pad;
                for tile[i] in root.tiles: Rectangle {
                    width: root.tile;
                    height: root.tile;
                    border-radius: 12px;
                    background: touch.pressed ? #ffffff44 : touch.has-hover ? #ffffff24 : transparent;
                    animate background { duration: 100ms; }

                    if tile.painted: Image {
                        source: tile.picture;
                        width: 28px;
                        height: 28px;
                        image-fit: contain;
                    }
                    if !tile.painted: Text {
                        text: tile.glyph;
                        color: #ffffff;
                        font-size: 26px;
                        horizontal-alignment: center;
                        vertical-alignment: center;
                    }
                    touch := TouchArea {
                        mouse-cursor: pointer;
                        clicked => { root.picked(i); }
                    }
                }
            }
        }
    }
}

/// The namespace of the panel's layer surface, for compositor rules.
pub const NAMESPACE: &str = "omime-sparkle-panel";
/// Its distance from the bottom and right edges of the screen, in logical pixels.
const MARGIN: i32 = 24;
/// How many emoji the panel keeps.
pub const RECENT: usize = 8;
/// What the panel shows before anything was inserted.
pub const FIRST_RECENT: [&str; RECENT] = ["😀", "😂", "👍", "🙏", "🎉", "❤️", "🚀", "✨"];

/// How large the emoji are painted, in pixels: enough for a scale of 2, and Slint draws
/// them smaller.
const PAINTED: u32 = 64;

/// The panel's side of the panel process.
struct View {
    ui: RecentPanel,
    overlay: Overlay,
    tiles: Rc<VecModel<Tile>>,
    painter: Option<Painter>,
    /// Emoji painted before, by glyph.
    pictures: HashMap<String, slint::Image>,
    shown: bool,
}

thread_local! {
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
}

impl View {
    fn tile(&mut self, glyph: &str) -> Tile {
        let picture = match self.pictures.get(glyph) {
            Some(picture) => Some(picture.clone()),
            None => {
                let painted = self.painter.as_mut().and_then(|p| p.paint(glyph, PAINTED));
                if let Some(picture) = &painted {
                    self.pictures.insert(glyph.to_owned(), picture.clone());
                }
                painted
            }
        };
        Tile {
            glyph: glyph.into(),
            painted: picture.is_some(),
            picture: picture.unwrap_or_default(),
        }
    }

    fn command(&mut self, line: &str) {
        match line.split_once(' ').unwrap_or((line, "")) {
            ("show", _) => self.shown = true,
            ("hide", _) => self.shown = false,
            ("recent", glyphs) => {
                let tiles: Vec<Tile> = glyphs
                    .split_whitespace()
                    .map(|glyph| self.tile(glyph))
                    .collect();
                self.tiles.set_vec(tiles);
            }
            _ => return,
        }
        if self.shown && self.tiles.row_count() > 0 {
            let width = self.ui.get_panel_width().round() as u32;
            let height = self.ui.get_panel_height().round() as u32;
            self.overlay.show(width, height);
        } else {
            self.overlay.hide();
        }
    }
}

/// Runs the panel process: `omime-sparkle --panel`.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let overlay = omime_overlay::install(
        NAMESPACE,
        Placement {
            bottom: Some(MARGIN),
            right: Some(MARGIN),
            ..Placement::default()
        },
        Pointer::Take,
    )?;
    let ui = RecentPanel::new()?;
    let tiles = Rc::new(VecModel::from(Vec::<Tile>::new()));
    ui.set_tiles(ModelRc::from(tiles.clone()));
    let picks = tiles.clone();
    ui.on_picked(move |i| {
        let Some(tile) = usize::try_from(i).ok().and_then(|i| picks.row_data(i)) else {
            return;
        };
        let mut stdout = std::io::stdout().lock();
        // The engine went away when this fails, and so will the panel.
        let _ = writeln!(stdout, "pick {}", tile.glyph).and_then(|()| stdout.flush());
    });
    let painter = Painter::new();
    if painter.is_none() {
        eprintln!("omime-sparkle --panel: no emoji font; the panel shows the emoji as text");
    }
    VIEW.with(|view| {
        view.replace(Some(View {
            ui: ui.clone_strong(),
            overlay,
            tiles,
            painter,
            pictures: HashMap::new(),
            shown: false,
        }))
    });

    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let _ = slint::invoke_from_event_loop(move || {
                VIEW.with(|view| {
                    if let Some(view) = &mut *view.borrow_mut() {
                        view.command(line.trim());
                    }
                });
            });
        }
        // The engine is gone.
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    // Shown for the toolkit; on the screen only once the engine says so.
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// The engine's side: the running panel process.
pub struct Panel {
    child: Child,
    stdin: ChildStdin,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("pid", &self.child.id())
            .finish()
    }
}

impl Panel {
    /// Starts the panel on the compositor at `display`. `pick` gets each emoji the user
    /// clicks there.
    pub fn start(display: &str, pick: impl Fn(String) + Send + 'static) -> std::io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--panel")
            .env("WAYLAND_DISPLAY", display)
            .env_remove("WAYLAND_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("the panel has no pipes"));
        };
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(glyph) = line.strip_prefix("pick ") {
                    pick(glyph.to_owned());
                }
            }
        });
        Ok(Self { child, stdin })
    }

    fn send(&mut self, line: &str) {
        // A panel that died shows nothing; Sparkle goes on without it.
        let _ = writeln!(self.stdin, "{line}").and_then(|()| self.stdin.flush());
    }

    pub fn show(&mut self, shown: bool) {
        self.send(if shown { "show" } else { "hide" });
    }

    pub fn set_recent(&mut self, recent: &[String]) {
        self.send(&format!("recent {}", recent.join(" ")));
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What Sparkle keeps across restarts: whether the panel is on, and the recent emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub panel: bool,
    pub recent: Vec<String>,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            panel: false,
            recent: FIRST_RECENT.iter().map(|&g| g.to_owned()).collect(),
        }
    }
}

impl Saved {
    /// `$XDG_STATE_HOME/oh-my-ime/sparkle`.
    fn path() -> Option<PathBuf> {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
        Some(state.join("oh-my-ime/sparkle"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// Lines `panel = true` and `recent = 🚀 ✨`; what is missing keeps its default.
    fn parse(text: &str) -> Self {
        let mut saved = Self::default();
        for line in text.lines() {
            match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("panel", value)) => saved.panel = value == "true",
                Some(("recent", value)) => {
                    let recent: Vec<String> = value
                        .split_whitespace()
                        .take(RECENT)
                        .map(str::to_owned)
                        .collect();
                    if !recent.is_empty() {
                        saved.recent = recent;
                    }
                }
                _ => {}
            }
        }
        saved
    }

    fn text(&self) -> String {
        format!(
            "panel = {}\nrecent = {}\n",
            self.panel,
            self.recent.join(" ")
        )
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let result = path
            .parent()
            .map_or(Ok(()), omime_proto::paths::ensure_private_dir)
            .and_then(|()| {
                omime_proto::paths::atomic_write(&path, self.text().as_bytes(), Some(0o600), false)
            });
        if let Err(e) = result {
            eprintln!("omime-sparkle: cannot save {}: {e}", path.display());
        }
    }

    /// Puts `glyph` first among the recent emoji.
    pub fn remember(&mut self, glyph: &str) {
        self.recent.retain(|g| g != glyph);
        self.recent.insert(0, glyph.to_owned());
        self.recent.truncate(RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_emoji_comes_first_and_the_oldest_goes() {
        let mut saved = Saved::default();
        saved.remember("🚀");
        assert_eq!(saved.recent[0], "🚀");
        assert_eq!(saved.recent.iter().filter(|g| *g == "🚀").count(), 1);
        saved.remember("🦀");
        assert_eq!(saved.recent[..2], ["🦀", "🚀"]);
        assert_eq!(saved.recent.len(), RECENT);
        assert!(!saved.recent.contains(&"✨".to_owned()), "the oldest went");
    }

    #[test]
    fn what_sparkle_keeps_reads_back() {
        let mut saved = Saved {
            panel: true,
            ..Saved::default()
        };
        saved.remember("🦀");
        assert_eq!(Saved::parse(&saved.text()), saved);
        assert_eq!(Saved::parse("panel = false\n"), Saved::default());
        assert_eq!(Saved::parse("garbage"), Saved::default());
    }
}
