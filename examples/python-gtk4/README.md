# Shout — a Python engine with GTK 4

An oh-my-ime engine written in Python with PyGObject, without the Rust SDK. It speaks
the engine protocol directly over D-Bus and draws its own GTK 4 window. The example
shows that the protocol is open and that GTK 4 windows work through the UI bridge.

It behaves like [`engines/echo`](../../engines/echo), with a window: typed `a`–`z`
letters compose a word. The text field shows the word as a lowercase preedit, while
the frameless green window shows it in UPPERCASE. Space or Return commits the word in
UPPERCASE and then reaches the application; Backspace edits the word, Escape cancels
it. A click on the window commits too. Each text field keeps its own word.

## Requirements

- Python 3
- PyGObject (`gi`)
- GTK 4

On NixOS, `nix develop .#e2e` provides all of them (the flake's end-to-end shell), and
on other distributions the distribution's `python3-gobject` and `gtk4` packages are
enough.

## Trying it

```sh
omime install examples/python-gtk4
omime switch shout
```

`omime install` copies the directory to `~/.local/share/oh-my-ime/engines/shout`; the
manifest runs `./shout.py` from there. `omime uninstall shout` removes it again. While
developing, omimed can find it in place instead: `$OMIME_ENGINE_PATH` takes
directories of engine directories, such as `examples`.

The engine needs the Wayland display omimed passes it, so start it through omimed, not
from a terminal. Its output goes to omimed's log under the engine's id.
