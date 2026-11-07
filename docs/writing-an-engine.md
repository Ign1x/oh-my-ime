# Writing an engine

English | [简体中文](zh-CN/writing-an-engine.md)

An oh-my-ime engine is an ordinary program. The daemon (`omimed`) starts it, sends it
the keys typed into the focused text field and applies what it answers: committed text,
an inline preedit, deletions. Its user interface is entirely its own; see
[Showing a window](#showing-a-window).

This guide uses the Rust SDK (`omime-sdk`). Engines in other languages speak the same
D-Bus protocol directly; see [Other languages](#other-languages).

## A complete engine

```rust
use std::collections::HashMap;
use omime_sdk::{Context, ContextId, Engine, KeyEvent, Preedit, modifiers};

#[derive(Default)]
struct Echo {
    words: HashMap<ContextId, String>,
}

impl Engine for Echo {
    fn activate(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn deactivate(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn reset(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        // Leave releases and shortcuts to the application.
        if key.released || key.modifiers & (modifiers::CONTROL | modifiers::ALT) != 0 {
            return false;
        }
        let word = self.words.entry(ctx.id()).or_default();
        match key.text.chars().next() {
            Some(c) if c.is_alphabetic() => {
                word.push(c);
                ctx.set_preedit(Preedit::with_cursor_at_end(word.clone()));
                true // consumed
            }
            _ => {
                if !word.is_empty() {
                    ctx.commit(std::mem::take(word).to_uppercase());
                }
                false // the application gets the key too, after the commit
            }
        }
    }
}

fn main() -> Result<(), omime_sdk::Error> {
    omime_sdk::run(Echo::default())
}
```

This is roughly [`engines/echo`](../engines/echo/src/main.rs). The rules worth knowing:

- Several contexts can be active at once. Key composition state by `ctx.id()`;
  activation, deactivation and reset affect only that context. Engine selection and
  popup placement currently remain shared across seats; the supported UI workflow
  is a single-seat session.
- `process_key` receives presses **and** releases. Return `true` to consume a key. A
  release is always routed like its press, whatever you return, so applications never
  see stuck keys.
- Actions requested through the `Context` (`commit`, `set_preedit`, `clear_preedit`,
  `delete_surrounding`, `forward_key`) are applied **in order, before** an unconsumed
  key reaches the application.
- Offsets and lengths into text are UTF-8 bytes, as in Wayland's text-input protocol.
  `Preedit::with_cursor_at_end` does the counting for you.
- Answer quickly. After 200 ms (configurable) the key is passed to the application.
  Further keys bypass the busy context until its late answer is discarded and a
  queued `reset` finishes. A reply abandoned on switching or deactivation is simply
  discarded and cannot reset the new session.
- `activate`, `deactivate`, `update` and `reset` tell you about focus and the field
  (`ctx.info()` has its content type, the surrounding text and, while your window is
  shown, the cursor rectangle). Wayland/Fcitx5 fields explicitly marked as sensitive
  bypass engines; XIM does not provide this information.
- Some applications do not show the preedit (xterm and other XIM applications that
  leave drawing it to the input method): `ctx.info().client_preedit` is `false` then,
  and an engine with a window shows what is being composed there. `frontend` says how
  the application reaches omimed (`wayland`, `xim`, `fcitx`).

## Installing it

An engine is a directory, named after its id, with an `engine.toml`:

```toml
[engine]
id = "echo"
name = "Echo"
description = "Commits every word in UPPERCASE."
languages = ["en"]
exec = ["omime-echo"]   # looked up in $PATH; "./bin/x" is relative to this directory
api = "1.0"
enabled_by_default = false

[engine.i18n.zh-CN]
name = "回声"
```

Keys omimed does not know are left out with a warning, so a manifest that uses keys of
a newer version still loads in an older one.

A manifest that ships with something else, for a program that may not be installed,
names the program in `requires = ["fcitx5"]` (names in `$PATH`, or absolute paths):
omimed offers the engine only where it is installed. `enabled_by_default = false`
makes it offered rather than enabled while `config.toml` lists no engines; users
enable it with `omime enable <id>` or **Add input method…** in the settings app.
Echo and Sparkle ship this way, as do the Fcitx 5 and IBus manifests. Omitting the
key enables an engine by default.

To install the directory for the current user, run `omime install <dir>`: it checks the
manifest, copies the directory to `~/.local/share/oh-my-ime/engines/<id>` and asks a
running omimed to reload. `--force` replaces an earlier copy, and `omime uninstall <id>`
removes it again.

omimed looks for engine directories in, in this order:

1. every directory in `$OMIME_ENGINE_PATH` (colon separated; handy while developing),
2. `~/.local/share/oh-my-ime/engines/`,
3. `oh-my-ime/engines/` under each `$XDG_DATA_DIRS` entry, e.g.
   `/usr/share/oh-my-ime/engines/`.

During development:

```sh
cargo build
PATH=$PWD/target/debug:$PATH OMIME_ENGINE_PATH=$PWD/engines omimed --engine echo
```

`--engine echo` tries Echo immediately. In another terminal, run `omime enable echo`
to include it in the hotkey cycle (`Ctrl+Space` toggles direct input,
`Ctrl+Shift+Space` cycles enabled engines). `omime switch <id>` can also select an
offered engine without enabling it.

What your engine prints to stdout and stderr ends up in omimed's log, one line at a
time, under the target `engine` and with your engine's id
(`INFO engine: … engine="echo"`).

## Showing a window

Create a window with any Wayland toolkit, frameless, sized to your content. omimed
starts your engine with `WAYLAND_DISPLAY` pointing at a private socket of its UI
bridge, so the toolkit needs no special code. Your toplevel becomes a popup next to the
text cursor whenever your engine is the active one.

- Your window stays hidden until you call `Host::set_popup_visible(true)`. Do not
  hide or close it to make it disappear; call `set_popup_visible(false)` and the
  platform unmaps it.
- Clicks on your window reach your engine as usual. React to them through the `Host`
  you get from `omime_sdk::spawn` (or `ctx.host()`), e.g. `host.commit(ctx_id, text)`.
- Toolkits that own the main thread: run the engine with `omime_sdk::spawn`, which
  returns once the engine is registered.
- For an ordinary window that should not be a popup (a settings dialog), connect to the
  display in `$OMIME_HOST_WAYLAND_DISPLAY` instead, e.g. from a separate process.
- A settings program in the manifest is what `omime settings <id>` and the tray menu
  open, as an ordinary window on the desktop:

  ```toml
  [engine.settings]
  exec = ["my-engine-settings"]   # resolved like `exec`
  bus = "session"                 # "engine": the private bus of a hosted input method
  restart = false                 # true: restart the engine when the program closes
  ```

The [Sparkle](../engines/sparkle) engine shows all of this with Slint.

## Floating panels

A window that stays on the desktop while the user types, such as a status bar, a soft
keyboard or a palette, is no popup: it belongs to the compositor in
`$OMIME_HOST_WAYLAND_DISPLAY` (`omime_sdk::ENV_HOST_WAYLAND_DISPLAY`), where it is a
layer surface (`zwlr_layer_shell_v1`) that does not take the keyboard
(`keyboard_interactivity` none), so the text field keeps it. One process usually cannot
be on two displays with one toolkit, so start a helper process for the panel with
`WAYLAND_DISPLAY` set to that display, and talk to it over a pipe; the engine stays the
one that commits text, with `Host::commit` for the field it serves.

- With Slint, `omime-overlay` makes a layer surface Slint's platform:
  `omime_overlay::install(namespace, placement, Pointer::Take)`, then show the component
  and call `Overlay::show(width, height)` and `Overlay::hide()`. It draws with Slint's
  software renderer, which draws text glyphs in one color: draw color emoji as images.
- Any other toolkit works too: gtk4-layer-shell, layer-shell-qt.
- Show the panel only while your engine serves a field (between `activate` and
  `deactivate`), and give the layer surface a namespace of your own, for compositor
  rules.

Sparkle's recent-emoji panel, which its menu turns on, does all of this: see
[`engines/sparkle/src/panel.rs`](../engines/sparkle/src/panel.rs).

## More than one input method

An engine can have input methods of its own, as a Rime engine has schemas. Announce
them, and the one in use, in `registered`. The daemon may restore a selection before
the registration reply reaches the SDK, so announce your current state. Users choose
these methods as `engine/id`
(`omime switch my-engine/fast`, the tray menu, `Ctrl+Shift+Space`, `engines` in
config.toml), and omimed asks your engine to switch:

```rust
impl omime_sdk::Engine for MyEngine {
    fn registered(&mut self, host: &omime_sdk::Host) {
        let fast = omime_sdk::InputMethodInfo {
            id: "fast".into(),
            name: "Fast".into(),
            ..Default::default()
        };
        host.set_input_methods(&[fast], self.mode.as_str());
    }

    fn select_input_method(&mut self, host: &omime_sdk::Host, id: &str) {
        self.mode = id.to_owned();
        host.set_current_input_method(id);
    }
    // ...
}
```

Call `set_current_input_method` too when the engine switches on its own. omimed keeps
the choice across restarts and gives it back to the engine when it starts.

## Remembering an input mode

Protocol 1.2 optionally lets the platform remember a mode within the same input
method, for example Chinese in chat and Latin letters in a terminal. Use stable,
untranslated IDs such as `native` and `latin`; keep display labels such as `中` and
`A` separate. The user enables this with both `apps.remember` and
`apps.remember_mode`; the latter defaults to false.

Advertise supported IDs with `Engine::input_modes`. Implement `set_input_mode` and
confirm the actual result through the context, after applying your engine's normal
composition policy. For an engine whose mode is a boolean, the relevant callbacks
look like this:

```rust
impl omime_sdk::Engine for MyEngine {
    fn input_modes(&self) -> Vec<String> {
        vec!["native".into(), "latin".into()]
    }

    fn set_input_mode(&mut self, ctx: &mut omime_sdk::Context<'_>, mode: &str) {
        self.latin = match mode {
            "native" => false,
            "latin" => true,
            _ => return,
        };
        ctx.report_input_mode(&self.schema, if self.latin { "latin" } else { "native" });
    }
    // ...
}
```

The method ID is local to your engine: pass the complete schema ID, such as
`double_pinyin_flypy`, rather than `rime/double_pinyin_flypy`; pass `""` if your engine
has no submethods. Report on activation and whenever keys or resets change the mode.
An empty reply does not confirm restoration. Unknown modes leave the engine unchanged.
For changes outside a callback, use `host.report_input_mode(ctx_id, method_id, mode)`
under the same state lock used by callbacks. `SetStatus` labels are presentation;
they do not confirm a mode.

The SDK assigns one increasing revision sequence to context actions and asynchronous
reports. The daemon uses the context, method, connection and revision to reject stale
observations, so an older key reply cannot undo a later menu change. Engines speaking
D-Bus directly advertise `"input-modes"` (`as`) in `Register`, return action kind 4
with `(method, mode, revision)` (`sst`), or call
`ReportInputMode(context, method, mode, revision)`. Allocate revisions starting at 1
when observing the actual state, before sending either form. Consult the XML protocol
for the signatures.

Existing 1.0/1.1 engines continue to work without mode restoration. The new SDK
suppresses mode reports when the daemon negotiates a version older than 1.2; direct
protocol clients must perform the same check on `Register`'s `api-minor` reply.

## A menu

Switches and actions, such as Chinese or English punctuation, go in the engine's menu,
which the platform shows (in its tray menu, for one) while your engine is in use.
Announce it, again whenever something in it changes, and act when an entry is
activated:

```rust
use omime_sdk::{Host, Property, PropertyKind};

impl MyEngine {
    fn menu(&self) -> Vec<Property> {
        vec![
            Property {
                id: "punct".into(),
                kind: PropertyKind::Toggle,
                label: "Chinese punctuation".into(),
                checked: self.chinese_punctuation,
                ..Default::default()
            },
            Property {
                id: "deploy".into(),
                label: "Deploy".into(),
                ..Default::default()
            },
        ]
    }
}

impl omime_sdk::Engine for MyEngine {
    fn registered(&mut self, host: &Host) {
        host.set_properties(&self.menu());
    }

    fn activate_property(&mut self, host: &Host, id: &str) {
        match id {
            "punct" => self.chinese_punctuation = !self.chinese_punctuation,
            "deploy" => self.deploy(),
            _ => return,
        }
        host.set_properties(&self.menu());
    }
    // ...
}
```

The menu is a flat list in menu order. An entry is an action (the default), a toggle
(`checked` says whether it is on), a radio entry (one choice among the entries with the
same `parent`), a menu (the `parent` of the entries in it) or a separator. While
developing, `omime properties` shows the menu of the engine in use and
`omime activate <id>` activates an entry.

## Other languages

The engine protocol is peer-to-peer D-Bus and is specified in
[`protocol/io.github.ign1x.OhMyIme.Engine1.xml`](../protocol/io.github.ign1x.OhMyIme.Engine1.xml).
An engine:

1. connects to the address in `$OMIME_ENGINE_ADDRESS` as a peer-to-peer connection,
2. exports `io.github.ign1x.OhMyIme.Engine1` at `/io/github/ign1x/OhMyIme/Engine`,
3. calls `Register($OMIME_ENGINE_ID, 1, 1, {})` on `/io/github/ign1x/OhMyIme`
   (`io.github.ign1x.OhMyIme.EngineHost1`); minor version 0 is fine for engines
   without input methods of their own and without a menu,
4. answers calls until the connection closes or `Shutdown` is called.

GLib (`g_dbus_connection_new_for_address`), Qt (`QDBusConnection::connectToPeer`),
sd-bus and zbus all support peer-to-peer connections.
[`examples/python-gtk4`](../examples/python-gtk4) is a whole engine in Python with
PyGObject, without the SDK, whose GTK 4 window becomes the popup like any other.
