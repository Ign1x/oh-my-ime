# Development

English | [简体中文](zh-CN/development.md)

## Environment

```sh
nix develop            # shared test tools plus cargo-deny and foot
nix develop .#e2e      # shared test tools plus Python/GTK 4 (used by CI)
```

Both shells provide the native libraries, Hyprland, headless Weston, a headless-capable sway with
XWayland, wtype, grim, dbus-daemon, Fcitx 5 and IBus for the end-to-end tests. The default
shell adds cargo-deny and foot; the e2e shell adds Python with PyGObject and GTK 4
for the Python engine example's test. Rust comes from rustup and `rust-toolchain.toml`.

## Checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/check-translations.py
cargo deny check
```

## End-to-end tests

The [localization guide](localization.md) covers the shared translation catalog.
`localization.rs` checks Chinese tray labels and diagnostics, stable control IDs,
and undoing a Chinese setup from an English session. Test processes default to
`C.UTF-8`; locale tests select their language per process.

`tests/e2e` starts a private headless sway, a private session bus and omimed for every
test, and types with a virtual keyboard into `omime-ti-client`, a tiny editor that
prints every text-input event. Nothing touches your desktop session.

The X11 tests also start XWayland (`TestEnv::start_with_x11`), which the dev shells
provide. `xim.rs` types into `omime-xim-client`, an X11 client that uses XIM, and
`fcitx.rs` drives the Fcitx5 module protocol with `omime-fcitx-client`. `popups.rs`
checks where engine windows open, that they stay hidden until the engine asks, how they
follow X11 text cursors, and that clicks on them reach the engine; it reads pixels with
`grim`. `hosted.rs` hosts `omime-im-client`, a small Wayland input method, as an engine;
`fcitx5.rs` and `ibus.rs` host the real Fcitx 5 and IBus from the shell. `tray.rs` checks
omimed's tray item with `omime-tray-host`, which plays a status bar's tray: it prints what
the item shows and clicks it and its menu.

`setup.rs` starts from an empty configuration to test preview, application and undo,
first-login choices, takeover of real Fcitx5/IBus processes, and Chinese input from
the first keystroke. It also validates both generated formats with Hyprland's
`--verify-config`.
`setup_safety.rs` uses headless Weston, which lacks input-method-v2, to check that
setup leaves a fake Fcitx5 owner running and omimed exits with status 69, including
when desktop notification delivery or bus authentication stalls. It combines late
unsupported replies with stalled authentication, with notifications both enabled
and disabled, and covers inherited sockets that point to a different compositor.
`setup.rs` checks that a
missing-addon failure releases the daemon before framework recovery, while a
launch-environment failure keeps working input running.
On sway, `setup_safety.rs` also checks setup with `LD_LIBRARY_PATH` removed and
startup with the compositor stopped for more than two seconds. Scripted Unix-socket
fixtures verify that an initial connection loss preserves the requested engine
and that seat names arriving with the initial sync are retained. The seat-name
test delays daemon startup and waits for a successful status request before
running doctor, so the diagnostic probe cannot take the fixture's connection.
Tests that stop a compositor always resume it during cleanup.

```sh
cargo build --workspace
cargo test -p omime-e2e -- --ignored
```

A failing test prints the end of `omimed.log` and `sway.log`. Set `OMIME_E2E_KEEP=1`
to keep each test's runtime directory with the full logs.

The same tests run against an installed build when `OMIME_E2E_INSTALL_DIR` points at its
`bin` directory. omimed then runs without the dev shell's `LD_LIBRARY_PATH`, so a
library the build cannot find fails the tests. CI does this with the Nix package:

```sh
nix build
cargo build -p omime-e2e
OMIME_E2E_INSTALL_DIR=$(readlink -f result)/bin cargo test -p omime-e2e -- --ignored
```

## Running omimed by hand

omimed needs a compositor with `zwp_input_method_v2` and `zwp_virtual_keyboard_v1`
(Hyprland, sway, ...) and no other input method running on it.

```sh
cargo build --workspace
PATH=$PWD/target/debug:$PATH OMIME_ENGINE_PATH=$PWD/engines \
  OMIME_LOG=debug omimed --engine echo
```

Useful environment variables:

| Variable | Effect |
|---|---|
| `OMIME_LOG` | Log filter, e.g. `debug`, `omimed::core=trace`; engines' own output is the target `engine` (`info,engine=warn` quiets it) |
| `OMIME_WL_DEBUG` | Log every Wayland message (includes what you type!) |
| `OMIME_ENGINE_PATH` | Extra engine directories, searched first |

### Nested Hyprland

To try things in Hyprland without leaving your current desktop, start it nested and
point clients at its socket:

```sh
nix develop
Hyprland &                      # opens a window; creates $XDG_RUNTIME_DIR/wayland-N
export WAYLAND_DISPLAY=wayland-N
unset GTK_IM_MODULE QT_IM_MODULE XMODIFIERS
omimed --engine echo &
foot                            # type into it
```

`grim` can take screenshots of the nested session. Because a nested Hyprland only draws
while its window is visible, create a headless output for unattended captures:
`hyprctl output create headless`.

Hyprland merges the modifiers and keys of all keyboards, which sway, the end-to-end
tests' compositor, does not. `tests/hyprland/stuck-modifiers.sh` checks in a nested
Hyprland that no modifier stays held after omimed lets go of the keyboard; it types with
`omime-keyboard --script`, which presses modifiers as keys like a physical keyboard:

```sh
cargo build --workspace
tests/hyprland/stuck-modifiers.sh
```
