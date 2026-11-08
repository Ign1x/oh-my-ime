# Contributing to oh-my-ime

Thanks for your interest! oh-my-ime is young, so design discussions are as welcome as code.

## Development environment

The Nix flake provides every native dependency and the compositors used for testing:

```sh
nix develop
cargo build --workspace
cargo test --workspace
```

Without Nix, install a Rust toolchain via rustup (the version is pinned in
`rust-toolchain.toml`), `pkg-config`, and the development files of `wayland`, `libxkbcommon`
and `dbus`.

## Before you send a change

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/check-translations.py
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
python3 -m unittest discover -s tests/desktop -p 'test_*.py'
cargo deny check        # licenses, advisories, sources
```

CI runs the same checks, plus the [headless end-to-end suite](docs/development.md#end-to-end-tests)
against source and installed Nix builds.

Binary release maintenance and the dry-run workflow are documented in
[docs/releases.md](docs/releases.md).

## Guidelines

- **Commits** follow [Conventional Commits](https://www.conventionalcommits.org/), for example
  `feat(wayland): forward modifiers to the virtual keyboard`. Keep each commit focused and
  compiling.
- **Errors:** libraries use `thiserror`, binaries use `anyhow`. Do not `unwrap()` or panic on
  daemon code paths; a crashing daemon means nobody can type.
- **Logging** uses `tracing`. Never log key contents or committed text unless explicitly
  requested for debugging, and never for sensitive fields.
- **Licensing:** the project is `MIT OR Apache-2.0`. Do not copy code from GPL/LGPL projects
  (Fcitx5, IBus, the wl-proxy example apps, …). Reading them for reference is fine.
- **Honesty:** documentation must describe what actually works. Use the ✅ / 🧩 / 🚧 / 📅
  markers from the README.
- **Docs:** user-facing changes update both `README.md` and `README.zh-CN.md`.
- **Translations:** mark user-facing messages and update the shared PO catalog;
  see [docs/localization.md](docs/localization.md).

## Architecture

Read [docs/architecture.md](docs/architecture.md) before touching the daemon.
