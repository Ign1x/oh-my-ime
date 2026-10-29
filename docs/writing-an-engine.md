# Writing an engine

An oh-my-ime engine is an ordinary program. The daemon (`omimed`) starts it, sends it
the keys typed into the focused text field and applies what it answers: committed text,
an inline preedit, deletions. Its user interface is entirely its own; see
[Showing a window](#showing-a-window).

This guide uses the Rust SDK (`omime-sdk`). Engines in other languages speak the same
D-Bus protocol directly; see [Other languages](#other-languages).

## A complete engine

```rust
use omime_sdk::{Context, Engine, KeyEvent, Preedit, modifiers};

#[derive(Default)]
struct Echo {
    word: String,
}

impl Engine for Echo {
    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        // Leave releases and shortcuts to the application.
        if key.released || key.modifiers & (modifiers::CONTROL | modifiers::ALT) != 0 {
            return false;
        }
        match key.text.chars().next() {
            Some(c) if c.is_alphabetic() => {
                self.word.push(c);
                ctx.set_preedit(Preedit::with_cursor_at_end(self.word.clone()));
                true // consumed
            }
            _ => {
                if !self.word.is_empty() {
                    ctx.commit(std::mem::take(&mut self.word).to_uppercase());
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

- `process_key` receives presses **and** releases. Return `true` to consume a key. A
  release is always routed like its press, whatever you return, so applications never
  see stuck keys.
- Actions requested through the `Context` (`commit`, `set_preedit`, `clear_preedit`,
  `delete_surrounding`, `forward_key`) are applied **in order, before** an unconsumed
  key reaches the application.
- Offsets and lengths into text are UTF-8 bytes, as in Wayland's text-input protocol.
  `Preedit::with_cursor_at_end` does the counting for you.
- Answer quickly. After 200 ms (configurable) the key is passed to the application and
  your late answer is discarded, followed by a `reset`.
- `activate`, `deactivate`, `update` and `reset` tell you about focus and the field
  (`ctx.info()` has its content type, the surrounding text and, while your window is
  shown, the cursor rectangle). Password and PIN fields never reach engines.

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

The [Sparkle](../engines/sparkle) engine shows all of this with Slint.

## Other languages

The engine protocol is peer-to-peer D-Bus and is specified in
[`protocol/io.github.ign1x.OhMyIme.Engine1.xml`](../protocol/io.github.ign1x.OhMyIme.Engine1.xml).
An engine:

1. connects to the address in `$OMIME_ENGINE_ADDRESS` as a peer-to-peer connection,
2. exports `io.github.ign1x.OhMyIme.Engine1` at `/io/github/ign1x/OhMyIme/Engine`,
3. calls `Register($OMIME_ENGINE_ID, 1, 0, {})` on `/io/github/ign1x/OhMyIme`
   (`io.github.ign1x.OhMyIme.EngineHost1`),
4. answers calls until the connection closes or `Shutdown` is called.

GLib (`g_dbus_connection_new_for_address`), Qt (`QDBusConnection::connectToPeer`),
sd-bus and zbus all support peer-to-peer connections.
