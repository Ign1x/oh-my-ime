#!/usr/bin/env bash
# Run from nix develop .#e2e. Each probe starts its own private desktop session.
# These probes assert desired behavior and fail on the audited commit d9899fc.
set -euo pipefail

audit_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$audit_dir/../../.." && pwd)
probe_target="$repo_dir/tests/e2e/tests/audit_probe.rs"

if [[ -e "$probe_target" ]]; then
  echo "Refusing to overwrite $probe_target" >&2
  exit 2
fi

cd -- "$repo_dir"
cargo build --workspace --locked
trap 'rm -f -- "$probe_target"' EXIT
cp -- "$audit_dir/regression-probes.rs" "$probe_target"
cargo test -p omime-e2e --locked --test audit_probe -- --ignored "$@"
