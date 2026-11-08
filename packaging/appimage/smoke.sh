#!/bin/sh
# Run in a fresh container after prepare-runtime.sh; no desktop bus is needed.
set -eu
[ "$#" -eq 1 ] || { printf 'Usage: %s OUTPUT_DIR\n' "$0" >&2; exit 2; }
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
exec python3 "$script_dir/smoke.py" "$1"
