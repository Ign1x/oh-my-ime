#!/usr/bin/env bash
# Checks in a nested Hyprland that no modifier stays held after omimed lets go of
# the keyboard.
#
# Hyprland merges the state of all keyboards; sway, the end-to-end tests'
# compositor, does not. With `share_states = 1` Hyprland also merges an input
# method's virtual keyboard whenever the input method holds no grab, so a Ctrl it
# kept would stay held for every application. Run it from `nix develop` in a
# Wayland session, after `cargo build --workspace`:
#
#   tests/hyprland/stuck-modifiers.sh
#
# OMIMED=path checks another omimed binary.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
bin=$repo/target/debug
omimed=${OMIMED:-$bin/omimed}
: "${WAYLAND_DISPLAY:?run this in a Wayland session}"
outer=$WAYLAND_DISPLAY
[[ $outer == /* ]] || outer=$XDG_RUNTIME_DIR/$outer

# Short: Hyprland's sockets must fit in a socket address.
dir=$(mktemp -d /tmp/omh.XXXXXX)
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  # Hyprland may run in a child of the process started here, and a nested one does
  # not always stop on SIGTERM.
  local hyprland
  hyprland=$(pgrep -f -- "Hyprland -c $dir/hyprland.conf" || true)
  if [[ -n $hyprland ]]; then
    kill $hyprland 2>/dev/null || true
    sleep 1
    kill -KILL $hyprland 2>/dev/null || true
  fi
  wait 2>/dev/null || true
  if [[ -n $dir && -d $dir ]]; then rm -rf -- "$dir"; fi
}
trap cleanup EXIT
mkdir -p "$dir/run" "$dir/config" "$dir/state" "$dir/data" "$dir/engines"
chmod 700 "$dir/run"

cat > "$dir/hyprland.conf" <<'EOF'
monitor = , 1280x720, auto, 1
misc {
  disable_hyprland_logo = true
  disable_splash_rendering = true
}
animations {
  enabled = false
}
xwayland {
  enabled = false
}
input {
  kb_layout = us
  virtualkeyboard {
    share_states = 1
  }
}
EOF

# Everything nested lives in its own runtime directory; Hyprland reaches the outer
# compositor by its socket's path.
export XDG_RUNTIME_DIR=$dir/run XDG_CONFIG_HOME=$dir/config XDG_STATE_HOME=$dir/state
export XDG_DATA_HOME=$dir/data OMIME_ENGINE_PATH=$dir/engines
unset DISPLAY GTK_IM_MODULE QT_IM_MODULE XMODIFIERS HYPRLAND_INSTANCE_SIGNATURE
WAYLAND_DISPLAY=$outer Hyprland -c "$dir/hyprland.conf" > "$dir/hyprland.log" 2>&1 &
pids+=($!)
for _ in $(seq 100); do
  [[ -S $dir/run/wayland-1 ]] && [[ -n $(ls "$dir/run/hypr" 2>/dev/null) ]] && break
  sleep 0.1
done
if [[ ! -S $dir/run/wayland-1 ]]; then
  echo "FAILED: Hyprland did not start"
  tail -5 "$dir/hyprland.log"
  exit 1
fi
export WAYLAND_DISPLAY=wayland-1
HYPRLAND_INSTANCE_SIGNATURE=$(ls "$dir/run/hypr")
export HYPRLAND_INSTANCE_SIGNATURE
for _ in $(seq 100); do hyprctl monitors > /dev/null 2>&1 && break; sleep 0.1; done

dbus-daemon --session --fork --print-address=1 --print-pid=1 \
  --address="unix:path=$dir/run/bus" > "$dir/bus"
export DBUS_SESSION_BUS_ADDRESS=unix:path=$dir/run/bus
pids+=("$(sed -n 2p "$dir/bus")")

# Waits until the command succeeds, for at most 10 s.
wait_for() {
  local what=$1
  shift
  for _ in $(seq 100); do
    "$@" && return 0
    sleep 0.1
  done
  echo "FAILED: timed out waiting for $what"
  if [[ -n ${OMIME_DEBUG:-} ]]; then
    pgrep -fa -- "Hyprland -c $dir" || echo "no Hyprland"
    grep -E "ERR|WARN" "$dir/run/hypr"/*/hyprland.log | tail -5
  fi
  exit 1
}
logged() { (( $(grep -c -- "$1" "$dir/omimed.log" || true) >= $2 )); }
has_window() { hyprctl clients | grep -q "class: $1\$"; }
key() { echo "$*" >&3; sleep 0.2; }

OMIME_LOG=debug "$omimed" > "$dir/omimed.log" 2>&1 &
pids+=($!)
wait_for "omimed to start" logged "starting input method session" 1
# A keyboard that presses modifiers as keys, as a physical one does.
mkfifo "$dir/keys"
"$bin/omime-keyboard" --script < "$dir/keys" > "$dir/keyboard.out" 2>&1 &
pids+=($!)
exec 3> "$dir/keys"
wait_for "the keyboard" grep -q ready "$dir/keyboard.out"

# A text field, then a window without one; each gets the focus when it opens.
"$bin/omime-ti-client" --app-id field --print-modifiers > "$dir/field.out" 2>&1 &
pids+=($!)
wait_for "the text field's window" has_window field
wait_for "omimed to take the text field" logged "keyboard grabbed" 1
"$bin/omime-ti-client" --app-id other --print-modifiers --no-text-field > "$dir/other.out" 2>&1 &
pids+=($!)
wait_for "the other window" has_window other
wait_for "omimed to let go of the keyboard" logged "keyboard released" 1
hyprctl dispatch focuswindow 'class:^(field)$' > /dev/null
wait_for "omimed to take the text field again" logged "keyboard grabbed" 2

# Hold Ctrl in the text field, move to the other window, let go of Ctrl there.
key press ctrl
hyprctl dispatch focuswindow 'class:^(other)$' > /dev/null
wait_for "omimed to let go of the keyboard again" logged "keyboard released" 2
key release ctrl
key tap x
sleep 0.3

last=$(grep '^modifiers ' "$dir/other.out" | tail -1)
if [[ $last == "modifiers 0 0 "* ]]; then
  echo "ok: no modifier stayed held ($last)"
else
  echo "FAILED: the window without a text field still sees modifiers: $last"
  exit 1
fi
