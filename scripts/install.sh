#!/usr/bin/env sh
# Builds oh-my-ime and installs it for the current user (or into $PREFIX).
#
#   scripts/install.sh            # installs into ~/.local
#   PREFIX=/usr/local scripts/install.sh
#   OMIME_INSTALL_BIN_DIR=/path/to/prebuilt/bin scripts/install.sh
#
# Installs omimed, omime, omime-panel, omime-settings (with its desktop entry) and omime-ibus,
# the bundled engines (echo, sparkle) and their manifests, and the manifests of Fcitx 5 and
# IBus, which omimed offers where they are installed.
set -eu

PREFIX=${PREFIX:-"$HOME/.local"}
case "$PREFIX" in
  /*) ;;
  *) PREFIX="$PWD/$PREFIX" ;;
esac
case "$PREFIX" in
  *:*) echo "PREFIX cannot contain ':' (the engine search-path separator)." >&2; exit 1 ;;
  *\\*|*\"*|*\$*) echo 'PREFIX cannot contain backslashes, double quotes or dollar signs (systemd executable-path restrictions).' >&2; exit 1 ;;
  *'
'*) echo "PREFIX cannot contain a newline." >&2; exit 1 ;;
esac
BIN="$PREFIX/bin"
ENGINES="$PREFIX/share/oh-my-ime/engines"
UNIT="$PREFIX/share/systemd/user/omimed.service"
cd "$(dirname "$0")/.."

if [ -n "${OMIME_INSTALL_BIN_DIR:-}" ]; then
  BUILD_BIN=$OMIME_INSTALL_BIN_DIR
else
  cargo build --release --locked --workspace --bins
  BUILD_BIN="${CARGO_TARGET_DIR:-target}/release"
fi

install -Dm755 "$BUILD_BIN/omimed" "$BIN/omimed"
install -Dm755 "$BUILD_BIN/omime" "$BIN/omime"
install -Dm755 "$BUILD_BIN/omime-ibus" "$BIN/omime-ibus"
install -Dm755 "$BUILD_BIN/omime-panel" "$BIN/omime-panel"
install -Dm755 "$BUILD_BIN/omime-settings" "$BIN/omime-settings"
# TOML and systemd quoted strings both escape backslashes and double quotes.
QUOTED_PREFIX=$(printf '%s' "$PREFIX" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')
install_manifest() {
  manifest_source=$1
  manifest_engine=$2
  manifest_binary=$3
  install -d "$ENGINES/$manifest_engine"
  while IFS= read -r manifest_line; do
    case "$manifest_line" in
      "exec = [\"$manifest_binary\"]") printf 'exec = ["%s/bin/%s"]\n' "$QUOTED_PREFIX" "$manifest_binary" ;;
      *) printf '%s\n' "$manifest_line" ;;
    esac
  done < "$manifest_source" > "$ENGINES/$manifest_engine/engine.toml"
  chmod 644 "$ENGINES/$manifest_engine/engine.toml"
}
for engine in echo sparkle; do
  install -Dm755 "$BUILD_BIN/omime-$engine" "$BIN/omime-$engine"
  install_manifest "engines/$engine/engine.toml" "$engine" "omime-$engine"
done
install -Dm644 data/engines/fcitx5/engine.toml "$ENGINES/fcitx5/engine.toml"
install_manifest data/engines/ibus/engine.toml ibus omime-ibus

# Launchers may not have the PATH of a shell either.
DESKTOP="$PREFIX/share/applications/io.github.ign1x.OhMyIme.Settings.desktop"
install -d "$(dirname "$DESKTOP")"
while IFS= read -r desktop_line; do
  case "$desktop_line" in
    Exec=*) printf 'Exec="%s/bin/omime-settings"\n' "$(printf '%s' "$QUOTED_PREFIX" | sed 's/%/%%/g')" ;;
    *) printf '%s\n' "$desktop_line" ;;
  esac
done < data/io.github.ign1x.OhMyIme.Settings.desktop > "$DESKTOP"
chmod 644 "$DESKTOP"

install -Dm644 data/config.toml "$PREFIX/share/oh-my-ime/config.toml"
for snippet in data/hyprland/*; do
  install -Dm644 "$snippet" "$PREFIX/share/oh-my-ime/hyprland/$(basename "$snippet")"
done

# The user manager may have no ~/.local/bin in PATH. Use absolute paths, escaping
# systemd specifiers.
SYSTEMD_PREFIX=$(printf '%s' "$QUOTED_PREFIX" | sed 's/%/%%/g')
install -d "$(dirname "$UNIT")"
while IFS= read -r unit_line; do
  case "$unit_line" in
    '[Service]') printf '%s\n' "$unit_line" "Environment=\"OMIME_ENGINE_PATH=$SYSTEMD_PREFIX/share/oh-my-ime/engines\"" ;;
    ExecStart=*) printf 'ExecStart="%s/bin/omimed"\n' "$SYSTEMD_PREFIX" ;;
    ExecReload=*) printf 'ExecReload="%s/bin/omime" reload\n' "$SYSTEMD_PREFIX" ;;
    *) printf '%s\n' "$unit_line" ;;
  esac
done < data/omimed.service > "$UNIT"
chmod 644 "$UNIT"

printf 'Installed into %s. Add %s to the session PATH.\n' "$PREFIX" "$BIN"
printf 'Session templates: %s/share/oh-my-ime/hyprland/\nUser service: %s\n' "$PREFIX" "$UNIT"
printf '%s\n' 'Preview Hyprland setup: omime setup --engine sparkle --take-over --dry-run'
printf '%s\n' 'Apply with --start in place of --dry-run. See docs/setup.md or docs/hyprland.md for manual configuration.'
