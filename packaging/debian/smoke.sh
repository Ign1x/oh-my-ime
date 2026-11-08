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
export DEBIAN_FRONTEND=noninteractive
# Minimal container images omit documentation; retain this package's files so
# the ownership and checksum checks exercise the complete installed payload.
printf '%s\n' 'path-include=/usr/share/doc/oh-my-ime/*' \
    'path-include=/usr/share/doc/oh-my-ime-rime/*' > /etc/dpkg/dpkg.cfg.d/zz-omime-smoke
apt-get update
apt-get install -y --no-install-recommends python3
mapfile -t packages < <(find "$packages_directory" -maxdepth 1 -type f -name 'oh-my-ime_*.deb' | sort)
[[ ${#packages[@]} == 1 ]] || { echo 'Expected exactly one platform package.' >&2; exit 1; }
names=(oh-my-ime)
if [[ "$components" == all ]]; then
    mapfile -t rime_packages < <(find "$packages_directory" -maxdepth 1 -type f -name 'oh-my-ime-rime_*.deb' | sort)
    [[ ${#rime_packages[@]} == 1 ]] || { echo 'Expected exactly one Rime package.' >&2; exit 1; }
    packages+=("${rime_packages[@]}")
    names+=(oh-my-ime-rime)
fi
for package in "${packages[@]}"; do
    dpkg-deb --info "$package"
    dpkg-deb --contents "$package"
    if dpkg-deb --ctrl-tarfile "$package" | tar -tf - | grep -Eq '(^|/)(preinst|postinst|prerm|postrm|config|triggers)$'; then
        echo 'Unexpected package hooks.' >&2
        exit 1
    fi
done
python3 "$scripts_directory/../check-install.py" snapshot /tmp/omime-install-state.json
apt-get install -y --no-install-recommends "${packages[@]}"
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
dpkg-query -W -f='${Package} ${Version} ${Architecture}\n${Depends}\n' "${names[@]}"
dpkg-query -L "${names[@]}"
verification=$(dpkg --verify "${names[@]}")
[[ -z "$verification" ]] || { printf '%s\n' "$verification" >&2; exit 1; }
python3 "$scripts_directory/../check-install.py" check "$components"
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
apt-get install -y --no-install-recommends --reinstall "${packages[@]}"
verification=$(dpkg --verify "${names[@]}")
[[ -z "$verification" ]] || { printf '%s\n' "$verification" >&2; exit 1; }
if [[ "$components" == all ]]; then
    dpkg --remove oh-my-ime-rime
    [[ ! -e /usr/bin/omime-rime ]]
    python3 "$scripts_directory/../check-install.py" check platform
    [[ -z $(dpkg --verify oh-my-ime) ]]
fi
dpkg --remove oh-my-ime
[[ ! -e /usr/bin/omimed && ! -e /usr/lib/systemd/user/omimed.service ]]
python3 "$scripts_directory/../check-install.py" unchanged /tmp/omime-install-state.json
dpkg --compare-versions 0.5.1~rc.1-1 lt 0.5.1-1
