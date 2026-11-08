#!/usr/bin/env bash
# Build and install in a disposable Arch Linux container, as root.
# Usage: smoke.sh RECIPE_ARCHIVE ARTIFACTS_DIR
set -euo pipefail
export LC_ALL=C

if [[ $# != 2 || $(id -u) != 0 ]]; then
  printf 'Usage (as root in an Arch container): %s RECIPE_ARCHIVE ARTIFACTS_DIR\n' "$0" >&2
  exit 2
fi
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
  printf 'Run this installation check in a disposable Arch Linux container.\n' >&2
  exit 2
fi

recipe=$(realpath "$1")
artifacts=$(realpath "$2")
scripts=$(cd -- "$(dirname -- "$0")" && pwd)
output=$(realpath -m "${OMIME_AUR_SMOKE_OUT:-$PWD/aur-smoke-results}")
work=$(mktemp -d /tmp/oh-my-ime-aur.XXXXXX)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$output"

pacman -Syu --needed --noconfirm base-devel python
python -m unittest discover -s "$scripts/../../scripts/tests" -p test_aur.py
if ! id omime-build >/dev/null 2>&1; then
  useradd --create-home omime-build
fi
tar -xzf "$recipe" -C "$work"
build="$work/oh-my-ime-bin"
# Each architecture uses the native build archives already available here,
# including when the future release URLs have not been published yet.
cp "$artifacts"/oh-my-ime-*linux.tar.gz "$build/"
chown -R omime-build:omime-build "$work"
cd "$build"
runuser -u omime-build -- makepkg --verifysource
sed -e 's/^CARCH=.*/CARCH="aarch64"/' \
  -e 's/^CHOST=.*/CHOST="aarch64-unknown-linux-gnu"/' \
  /etc/makepkg.conf > "$work/makepkg-aarch64.conf"
runuser -u omime-build -- makepkg --config "$work/makepkg-aarch64.conf" \
  --ignorearch --verifysource
runuser -u omime-build -- makepkg --printsrcinfo > "$output/makepkg.SRCINFO"
python "$scripts/verify.py" metadata .SRCINFO "$output/makepkg.SRCINFO"
runuser -u omime-build -- makepkg --cleanbuild --noconfirm
pacman -U --noconfirm ./oh-my-ime-bin-*-x86_64.pkg.tar.zst ./oh-my-ime-rime-bin-*-x86_64.pkg.tar.zst
pacman -Qi oh-my-ime-bin oh-my-ime-rime-bin
pacman -Qkk oh-my-ime-bin oh-my-ime-rime-bin
python "$scripts/verify.py" runtime

# These packages only copy release binaries. AArch64 packaging can therefore
# run on x86_64; runtime execution still needs a native AArch64 machine.
runuser -u omime-build -- makepkg --config "$work/makepkg-aarch64.conf" \
  --ignorearch --cleanbuild --noconfirm
for package in ./*-aarch64.pkg.tar.zst; do
  bsdtar -xOf "$package" .PKGINFO | tee "$output/$(basename "$package").PKGINFO"
  bsdtar -xOf "$package" .PKGINFO | grep -qx 'arch = aarch64'
  destination=$(mktemp -d "$work/arm.XXXXXX")
  bsdtar -xf "$package" -C "$destination"
  for program in "$destination"/usr/bin/*; do
    readelf -h "$program" | grep 'Machine:.*AArch64'
  done
done
# Reinstall the same version, then remove only the optional engine.
pacman -U --noconfirm ./oh-my-ime-bin-*-x86_64.pkg.tar.zst ./oh-my-ime-rime-bin-*-x86_64.pkg.tar.zst
python "$scripts/verify.py" removal
cp ./*.pkg.tar.zst PKGBUILD .SRCINFO "$output/"
printf 'AUR source, metadata, installation, and architecture checks passed.\n'
