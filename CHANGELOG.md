# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
- End-to-end tests in a private headless sway session, run in CI.
