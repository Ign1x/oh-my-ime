#!/bin/sh
# Run in a fresh Ubuntu 24.04 container with python3; no desktop bus is needed.
set -eu
[ "$#" -eq 1 ] || { printf 'Usage: %s OUTPUT_DIR\n' "$0" >&2; exit 2; }
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
exec python3 "$script_dir/smoke.py" "$1"
