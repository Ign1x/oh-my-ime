#!/usr/bin/env bash
# Install documented host runtimes in a disposable AppImage smoke container.
set -euo pipefail
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
    echo 'Run this script in a disposable packaging container.' >&2
    exit 1
fi
graphics_test=false
case "${1:-}" in
    '') ;;
    --graphics-test) graphics_test=true ;;
    *) echo "Usage: $0 [--graphics-test]" >&2; exit 2 ;;
esac
# shellcheck source=/dev/null
source /etc/os-release
case "$ID" in
    ubuntu|debian)
        packages=(python3 libgcc-s1 libstdc++6 libunwind8 libfontconfig1 libxkbcommon0
            libwayland-client0 libwayland-cursor0 libwayland-egl1 libegl1 libgl1 libgles2)
        if "$graphics_test"; then
            packages+=(weston dbus-daemon libgl1-mesa-dri fonts-dejavu-core)
        fi
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y --no-install-recommends "${packages[@]}"
        ;;
    fedora)
        packages=(python3 libgcc libstdc++ libunwind fontconfig libxkbcommon
            libwayland-client libwayland-cursor libwayland-egl libglvnd-egl libglvnd-glx libglvnd-gles)
        if "$graphics_test"; then
            packages+=(weston dbus-daemon mesa-dri-drivers mesa-libEGL mesa-libGL dejavu-sans-fonts)
        fi
        dnf install -y --setopt=install_weak_deps=False "${packages[@]}"
        ;;
    arch)
        packages=(python gcc-libs libunwind fontconfig libxkbcommon wayland libglvnd)
        if "$graphics_test"; then
            packages+=(weston dbus mesa ttf-dejavu)
        fi
        pacman -Syu --noconfirm --needed "${packages[@]}"
        ;;
    *) echo "Unsupported smoke-test distribution: $ID" >&2; exit 1 ;;
esac
