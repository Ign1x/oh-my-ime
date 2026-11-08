#!/usr/bin/env bash
# Install build dependencies in a Fedora 43 packaging container.
set -euo pipefail
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
    echo 'Run this script in a disposable packaging container.' >&2
    exit 1
fi
dnf install -y --setopt=install_weak_deps=False \
    python3 rpm-build binutils \
    libxkbcommon libxkbcommon-x11 libwayland-client libwayland-cursor libwayland-egl fontconfig freetype \
    libglvnd-egl libglvnd-glx libglvnd-gles vulkan-loader \
    libX11 libXcursor libXi libXrandr librime
