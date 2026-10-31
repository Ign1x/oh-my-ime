#!/usr/bin/env sh
# Builds oh-my-ime and installs it for the current user (or into $PREFIX).
#
#   scripts/install.sh            # installs into ~/.local
#   PREFIX=/usr/local scripts/install.sh
#
# Installs omimed, omime and omime-ibus, the bundled engines (echo, sparkle) and their
# manifests, and the manifests of Fcitx 5 and IBus for `omime install`.
set -eu

PREFIX=${PREFIX:-"$HOME/.local"}
BIN="$PREFIX/bin"
ENGINES="$PREFIX/share/oh-my-ime/engines"
cd "$(dirname "$0")/.."

cargo build --release --locked --workspace --bins

install -Dm755 target/release/omimed "$BIN/omimed"
install -Dm755 target/release/omime "$BIN/omime"
install -Dm755 target/release/omime-ibus "$BIN/omime-ibus"
for engine in echo sparkle; do
  install -Dm755 "target/release/omime-$engine" "$BIN/omime-$engine"
  install -Dm644 "engines/$engine/engine.toml" "$ENGINES/$engine/engine.toml"
done
# Not on the engine search path: omimed would list them where they are not installed.
for engine in fcitx5 ibus; do
  install -Dm644 "data/engines/$engine/engine.toml" \
    "$PREFIX/share/oh-my-ime/extra-engines/$engine/engine.toml"
done

echo "Installed into $PREFIX. Make sure $BIN is in your PATH, then see docs/hyprland.md."
