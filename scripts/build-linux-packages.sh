#!/usr/bin/env bash
# Native runners invoke disposable distro containers; nothing is installed on the host.
# Usage: scripts/build-linux-packages.sh MANIFEST ARTIFACTS OUTPUT SYSTEM
set -euo pipefail
task_source=$(cd -- "$(dirname -- "$0")/.." && pwd)
task_manifest=$(realpath -- "$1")
task_artifacts=$(realpath -- "$2")
mkdir -p -- "$3"
task_output=$(realpath -- "$3")
task_system=$4
case "$(uname -m):$task_system" in
  x86_64:x86_64-linux|aarch64:aarch64-linux) ;;
  *) echo "Use a native runner for $task_system" >&2; exit 1 ;;
esac

container() {
  local task_image=$1
  shift
  local task_log task_status
  task_log=$(mktemp)
  set +e
  docker run --rm \
    --mount "type=bind,src=$task_source,dst=/source,readonly" \
    --mount "type=bind,src=$task_artifacts,dst=/artifacts,readonly" \
    --mount "type=bind,src=$task_manifest,dst=/release-input.json,readonly" \
    --mount "type=bind,src=$task_output,dst=/out" \
    -e "OMIME_PACKAGE_SYSTEM=$task_system" "$task_image" "$@" 2>&1 | tee "$task_log"
  task_status=${PIPESTATUS[0]}
  set -e
  if [ "$task_status" -ne 0 ]; then
    python3 - "$task_image" "$task_log" <<'PY'
from pathlib import Path
import sys
tail = '\n'.join(Path(sys.argv[2]).read_text(errors='replace').splitlines()[-60:])
escaped = tail.replace('%', '%25').replace('\r', '%0D').replace('\n', '%0A')
print(f'::error title=Packaging container {sys.argv[1]}::{escaped}')
PY
  fi
  rm -f -- "$task_log"
  return "$task_status"
}

# shellcheck disable=SC2016 # Expanded inside the container, after its environment is set.
container ubuntu:24.04 bash -euo pipefail -c '
  bash /source/packaging/debian/prepare.sh
  python3 /source/scripts/package-linux.py --format deb --manifest /release-input.json \
    --artifacts /artifacts --system "$OMIME_PACKAGE_SYSTEM" --out /out
  bash /source/packaging/debian/smoke.sh /out platform
'
# Noble's librime is too old for the optional Rime engine; verify the complete
# package installation in a distribution providing librime >= 1.11.
container debian:13 bash -euo pipefail -c '
  bash /source/packaging/debian/smoke.sh /out all
'
# shellcheck disable=SC2016 # Expanded inside the container.
container fedora:43 bash -euo pipefail -c '
  bash /source/packaging/rpm/prepare.sh
  python3 /source/scripts/package-linux.py --format rpm --manifest /release-input.json \
    --artifacts /artifacts --system "$OMIME_PACKAGE_SYSTEM" --out /out
  bash /source/packaging/rpm/smoke.sh /out all
'
# shellcheck disable=SC2016 # Expanded inside the container.
container ubuntu:24.04 bash -euo pipefail -c '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends \
    python3 ca-certificates patchelf squashfs-tools binutils \
    build-essential cmake ninja-build libboost-dev libboost-regex-dev \
    libgoogle-glog-dev libgflags-dev libleveldb-dev libmarisa-dev libopencc-dev \
    libyaml-cpp-dev libunwind-dev libx11-dev \
    librime-data rime-data-double-pinyin rime-data-luna-pinyin libopencc-data \
    xkb-data libxkbcommon0 libfontconfig1 libwayland-client0 libwayland-cursor0 libwayland-egl1
  python3 /source/packaging/appimage/prepare-rime.py --prefix /tmp/omime-appimage-rime
  export OMIME_APPIMAGE_RIME_PREFIX=/tmp/omime-appimage-rime
  python3 /source/scripts/package-linux.py --format appimage --manifest /release-input.json \
    --artifacts /artifacts --system "$OMIME_PACKAGE_SYSTEM" --out /out
'
container ubuntu:24.04 bash -euo pipefail -c '
  bash /source/packaging/appimage/prepare-runtime.sh
  apt-get install -y --no-install-recommends util-linux
  setpriv --reuid=1000 --regid=1000 --clear-groups \
    bash /source/packaging/appimage/smoke.sh /out
'
# Exercise the host Mesa/LLVM path on a newer distribution, including Rime's UI.
# shellcheck disable=SC2016 # Expanded inside the container.
container fedora:43 bash -euo pipefail -c '
  bash /source/packaging/appimage/prepare-runtime.sh --graphics-test
  dnf install -y --setopt=install_weak_deps=False util-linux
  mkdir -p "/out/validation/$OMIME_PACKAGE_SYSTEM/fedora-egl"
  chown 65534:65534 "/out/validation/$OMIME_PACKAGE_SYSTEM/fedora-egl"
  setpriv --reuid=65534 --regid=65534 --clear-groups \
    python3 /source/packaging/appimage/graphics-smoke.py /out \
      "/out/validation/$OMIME_PACKAGE_SYSTEM/fedora-egl"
'
