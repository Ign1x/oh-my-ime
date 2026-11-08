#!/usr/bin/env bash
# Install build dependencies in an Ubuntu 24.04 packaging container.
set -euo pipefail
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
    echo 'Run this script in a disposable packaging container.' >&2
    exit 1
fi
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    python3 dpkg-dev binutils xz-utils \
    libxkbcommon0 libwayland-client0 libfontconfig1 libfreetype6 \
    libegl1 libgl1 libgles2 libvulkan1 libx11-6 libxcursor1 libxi6 libxrandr2 \
    libx11-xcb1 libxkbcommon-x11-0 libwayland-cursor0 libwayland-egl1 librime1t64
