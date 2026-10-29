# Development

English | [简体中文](zh-CN/development.md)

## Environment

```sh
nix develop            # everything, including Hyprland for manual tests
nix develop .#e2e      # only what the end-to-end tests need (used by CI)
```

The shells provide the native libraries, a headless-capable sway, wtype and
dbus-daemon. Rust comes from rustup and `rust-toolchain.toml`.

## Checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

## End-to-end tests

`tests/e2e` starts a private headless sway, a private session bus and omimed for every
test, and types with a virtual keyboard into `omime-ti-client`, a tiny editor that
prints every text-input event. Nothing touches your desktop session.

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
| `OMIME_LOG` | Log filter, e.g. `debug`, `omimed::core=trace` |
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
