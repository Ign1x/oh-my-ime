#!/usr/bin/env bash
# Usage: smoke.sh OUTPUT_DIRECTORY [platform|all]
set -euo pipefail
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
    echo 'Run this script in a disposable test container.' >&2
    exit 1
fi
packages_directory=$(realpath "${1:?output directory is required}")
components=${2:-all}
case "$components" in platform|all) ;; *) exit 2 ;; esac
scripts_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
dnf install -y --setopt=install_weak_deps=False python3
mapfile -t packages < <(find "$packages_directory" -maxdepth 1 -type f -name 'oh-my-ime-[0-9]*.rpm' | sort)
[[ ${#packages[@]} == 1 ]] || { echo 'Expected exactly one platform package.' >&2; exit 1; }
names=(oh-my-ime)
if [[ "$components" == all ]]; then
    mapfile -t rime_packages < <(find "$packages_directory" -maxdepth 1 -type f -name 'oh-my-ime-rime-[0-9]*.rpm' | sort)
    [[ ${#rime_packages[@]} == 1 ]] || { echo 'Expected exactly one Rime package.' >&2; exit 1; }
    packages+=("${rime_packages[@]}")
    names+=(oh-my-ime-rime)
fi
for package in "${packages[@]}"; do
    rpm -qip "$package"
    rpm -qlp "$package"
    rpm -qp --requires "$package"
    [[ -z $(rpm -qp --scripts "$package") ]] || { echo 'Unexpected package hooks.' >&2; exit 1; }
    rpm -qp --requires "$package" | grep -Eq '^libc\.so\.6\(GLIBC_[0-9]'
done
python3 "$scripts_directory/../check-install.py" snapshot /tmp/omime-install-state.json
dnf install -y --setopt=install_weak_deps=False "${packages[@]}"
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
rpm -q --qf '%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\n' "${names[@]}"
rpm -ql "${names[@]}"
rpm -V "${names[@]}"
python3 "$scripts_directory/../check-install.py" check "$components"
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
rpm -Uvh --replacepkgs "${packages[@]}"
rpm -V "${names[@]}"
if [[ "$components" == all ]]; then
    rpm -e oh-my-ime-rime
    [[ ! -e /usr/bin/omime-rime ]]
    python3 "$scripts_directory/../check-install.py" check platform
    rpm -V oh-my-ime
fi
rpm -e oh-my-ime
[[ ! -e /usr/bin/omimed && ! -e /usr/lib/systemd/user/omimed.service ]]
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
