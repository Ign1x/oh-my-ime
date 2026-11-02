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

[engine.i18n.zh-CN]
name = "回声"
```

Keys omimed does not know are left out with a warning, so a manifest that uses keys of
a newer version still loads in an older one.

A manifest that ships with something else, for a program that may not be installed,
names the program in `requires = ["fcitx5"]` (names in `$PATH`, or absolute paths):
omimed offers the engine only where it is installed. `enabled_by_default = false`
makes it offered rather than enabled while `config.toml` lists no engines; users
enable it with `omime enable <id>`.

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

Then switch with the hotkeys (`Ctrl+Space` toggles direct input, `Ctrl+Shift+Space`
cycles engines) or with `omime switch <id>`.

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

## More than one input method

An engine can have input methods of its own, as a Rime engine has schemas. Announce
them, and the one in use, in `registered`, which runs before any other call, so that
what it says is up to date; users then choose them as `engine/id`
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
