# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Applications using the Fcitx5 GTK or Qt input method module (`GTK_IM_MODULE=fcitx`,
  `QT_IM_MODULE=fcitx`), such as X11 applications under XWayland, type through omimed's
  engines: omimed serves the Fcitx5 module protocol under `org.fcitx.Fcitx5` and
  `org.freedesktop.portal.Fcitx` whenever fcitx5 does not hold those names. Engine
  windows do not follow these applications' text cursor yet.

## [0.2.0] - 2026-10-30

### Added

- Per-application engines: with `remember = true` in the new `[apps]` section of
  config.toml, each application gets back the engine last chosen in it, also after a
  restart. omimed learns the focused application from the compositor's
  `zwlr_foreign_toplevel_manager_v1`, and engines now receive its app id (`app-id`).
- `omime doctor` explains what is wrong with a setup, one line per finding: whether
  omimed runs and holds every seat (and which other input method holds one), whether
  the compositor reports focused windows, `config.toml`, broken engine manifests,
  engines that cannot start and input method variables in the environment. The
  control API's new `Diagnose` method provides the daemon's part.
- `omime status --watch [--json]` prints one line per change of the status, also while
  omimed stops and starts, for status bars such as waybar; the control API's new
  `StatusChanged` signal carries the changes.
- `omime install <dir>` copies an engine directory into
  `~/.local/share/oh-my-ime/engines` after checking its manifest, and
  `omime uninstall <id>` removes it again; both work without a running omimed.
- Simplified Chinese versions of the guides in `docs/zh-CN`, linked from README.zh-CN.md.
- A Nix package: `nix profile install github:Ign1x/oh-my-ime`, or the flake's package and
  overlay in a NixOS configuration. It contains the systemd user unit, and works without
  a dev shell: omimed finds the bundled engines and Sparkle its graphics libraries. CI
  runs the end-to-end tests against it.

### Fixed

- Keys typed with Control held no longer fail to reach the engine. xkb types NUL for
  Ctrl+Space, which D-Bus strings cannot carry; `KeyEvent::text` now never contains
  control characters.

### Changed

- Warnings about failed or late engine requests name the request.

## [0.1.0] - 2026-10-29

The first release: a working input method platform for Hyprland and other
compositors with `input-method-v2`, with engines that draw their own UI.

### Added

- Project foundation: Cargo workspace, pinned toolchain, Nix dev shell with test compositors,
  CI (fmt, clippy, tests, cargo-deny), bilingual README and architecture documentation.
- `omime-proto`: D-Bus names, protocol version and wire types, engine manifests, the
  configuration file and XDG paths.
- `omimed`: the daemon.
  - `input-method-v2` frontend with keyboard grab, virtual keyboard passthrough, key
    repeat for consumed keys, and the commit/preedit rules Hyprland needs.
  - UI bridge: engine windows become IME popups through a multiplexing Wayland proxy,
    with a virtual `xdg_shell`, pointer routing, held buffers while hidden, shm
    validation, dependency-ordered cleanup and reconnection.
  - Engine host: discovery, launch, supervision with crash restarts, startup and key
    timeouts, ordered per-field request queues.
  - Switching with hotkeys (`Ctrl+Space`, `Ctrl+Shift+Space`) and the
    `io.github.ign1x.OhMyIme.Daemon1` control API; single instance per session.
  - Password and PIN fields bypass engines and the keyboard grab.
- Engine protocol v1 (`protocol/io.github.ign1x.OhMyIme.Engine1.xml`) and the Rust SDK
  `omime-sdk`.
- `omime` CLI: `list`, `status [--json]`, `switch`, `next`, `toggle`.
- `echo` engine for tests and as a tutorial.
- `sparkle` demo engine: emoji by `:shortcode` from an animated deck drawn with Slint,
  with mouse selection and an arrow that follows the text cursor.
- UI bridge: engine windows are hidden until their engine calls `SetPopupVisible(true)`;
  the last fractional scale is handed to new engine windows.
- End-to-end tests in a private headless sway session, run in CI, including slow and
  misbehaving engines and a lost compositor connection.
- omimed remembers the engine in use across restarts; `omime enable`, `omime disable`
  (editing `config.toml` in place) and `omime reload`.
- `scripts/install.sh`, Hyprland snippets (Lua and legacy), a systemd user unit, a sample
  configuration and the Hyprland guide (`docs/hyprland.md`).

[Unreleased]: https://github.com/Ign1x/oh-my-ime/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ign1x/oh-my-ime/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ign1x/oh-my-ime/releases/tag/v0.1.0
