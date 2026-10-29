#!/usr/bin/env sh
# Builds oh-my-ime and installs it for the current user (or into $PREFIX).
#
#   scripts/install.sh            # installs into ~/.local
#   PREFIX=/usr/local scripts/install.sh
#
# Installs omimed and omime, the bundled engines (echo, sparkle) and their manifests.
set -eu

PREFIX=${PREFIX:-"$HOME/.local"}
BIN="$PREFIX/bin"
ENGINES="$PREFIX/share/oh-my-ime/engines"
cd "$(dirname "$0")/.."

cargo build --release --locked --workspace --bins

install -Dm755 target/release/omimed "$BIN/omimed"
install -Dm755 target/release/omime "$BIN/omime"
for engine in echo sparkle; do
  install -Dm755 "target/release/omime-$engine" "$BIN/omime-$engine"
  install -Dm644 "engines/$engine/engine.toml" "$ENGINES/$engine/engine.toml"
done

echo "Installed into $PREFIX. Make sure $BIN is in your PATH, then see docs/hyprland.md."
