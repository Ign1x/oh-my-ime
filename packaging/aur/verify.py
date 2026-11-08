#!/usr/bin/env python3
"""Checks used by smoke.sh inside a disposable Arch Linux container."""

import argparse
import ctypes
import os
from pathlib import Path
import pwd
import subprocess
import tempfile
import tomllib


def package_version(version):
    base, separator, prerelease = version.partition("-")
    return f"{base}pre{prerelease.replace('-', '_')}" if separator else base


def check_metadata(expected, actual):
    def parse(path):
        sections, current = {}, None
        for line in Path(path).read_text().splitlines():
            if not line.strip():
                continue
            key, value = line.strip().split(" = ", 1)
            if key in ("pkgbase", "pkgname"):
                current = {}
                sections[(key, value)] = current
            else:
                current.setdefault(key, []).append(value)
        return sections

    generated, printed = parse(expected), parse(actual)
    if generated != printed:
        raise RuntimeError(f"generated .SRCINFO disagrees with makepkg:\n{generated}\n{printed}")
    print("Generated .SRCINFO matches makepkg --printsrcinfo")


def check_runtime():
    programs = (
        "omimed", "omime", "omime-ibus", "omime-panel", "omime-settings",
        "omime-echo", "omime-sparkle", "omime-rime",
    )
    for name in programs:
        binary = Path("/usr/bin") / name
        linked = subprocess.check_output(["ldd", str(binary)], text=True)
        print(f"{name}:\n{linked}")
        if "not found" in linked:
            raise RuntimeError(f"unresolved libraries: {name}")
    # Slint loads these libraries at runtime, outside the ELF dependency table.
    for library in (
        "libfontconfig.so.1", "libfreetype.so.6", "libEGL.so.1", "libGL.so.1",
        "libxkbcommon.so.0", "libX11.so.6", "libXcursor.so.1", "libXi.so.6",
        "libXrandr.so.2", "libvulkan.so.1", "libwayland-client.so.0", "librime.so.1",
    ):
        ctypes.CDLL(library)
        print(f"dlopen: {library}")
    daemon = subprocess.check_output(["omimed", "--version"], text=True).strip()
    version = daemon.removeprefix("omimed ")
    assert daemon == f"omimed {version}" and version[0].isdigit(), daemon
    assert subprocess.check_output(["omime-settings", "--version"], text=True).strip() == f"omime-settings {version}"
    pkgver = package_version(version)
    if "-" in version:
        assert subprocess.check_output(["vercmp", pkgver, version.partition("-")[0]], text=True).strip() == "-1"
    for package in ("oh-my-ime-bin", "oh-my-ime-rime-bin"):
        installed = subprocess.check_output(["pacman", "-Q", package], text=True).strip()
        assert installed == f"{package} {pkgver}-1", installed
    subprocess.run(["omime", "help"], check=True)
    subprocess.run(["omime-settings", "--help"], check=True)

    for name in ("echo", "sparkle", "ibus", "rime", "fcitx5"):
        manifest = Path("/usr/share/oh-my-ime/engines") / name / "engine.toml"
        engine = tomllib.loads(manifest.read_text())["engine"]
        if name != "fcitx5":
            assert engine["exec"][0] == f"/usr/bin/omime-{name}"
            assert os.access(engine["exec"][0], os.X_OK)
        if name == "rime":
            assert engine["settings"]["exec"] == ["/usr/bin/omime-rime", "--settings"]
        print(f"manifest: {name}")
    service = Path("/usr/lib/systemd/user/omimed.service").read_text()
    assert "ExecStart=/usr/bin/omimed" in service
    assert "ExecReload=/usr/bin/omime reload" in service
    assert not Path("/usr/share/systemd/user/omimed.service").exists()
    desktop = Path("/usr/share/applications/io.github.ign1x.OhMyIme.Settings.desktop")
    assert "Exec=/usr/bin/omime-settings" in desktop.read_text()
    owned = {}
    for package in ("oh-my-ime-bin", "oh-my-ime-rime-bin"):
        paths = subprocess.check_output(["pacman", "-Qql", package], text=True).splitlines()
        owned[package] = {path for path in paths if not path.endswith("/")}
        assert all(path.startswith("/usr/") for path in paths)
        assert not any("/hooks/" in path for path in paths)
        assert Path(f"/usr/share/licenses/{package}/LICENSE-MIT").is_file()
        print(f"owned: {package}: {len(paths)} paths")
    assert not owned["oh-my-ime-bin"] & owned["oh-my-ime-rime-bin"]
    for name in (
        "default.yaml", "essay.txt", "luna_pinyin.schema.yaml",
        "luna_pinyin.dict.yaml", "double_pinyin_flypy.schema.yaml",
    ):
        data = Path("/usr/share/rime-data") / name
        assert data.is_file(), data
        subprocess.run(["pacman", "-Qo", str(data)], check=True)
    # Exercise the installed librime and the same default schemas as omime-rime.
    with tempfile.TemporaryDirectory(prefix="oh-my-ime-rime-") as temporary:
        user = Path(temporary)
        (user / "build").mkdir()
        (user / "default.custom.yaml").write_text(
            "patch:\n  schema_list:\n    - schema: double_pinyin_flypy\n    - schema: luna_pinyin\n"
        )
        subprocess.run([
            "rime_deployer", "--build", str(user), "/usr/share/rime-data", str(user / "build"),
        ], check=True, timeout=180)
        # Arch can ship the compiled dictionary in the shared data directory;
        # deployment still builds the user's schema and spelling-index files.
        for name in (
            "luna_pinyin.prism.bin", "luna_pinyin.schema.yaml",
            "double_pinyin_flypy.prism.bin", "double_pinyin_flypy.schema.yaml",
        ):
            assert (user / "build" / name).is_file(), name
        assert any((directory / "luna_pinyin.table.bin").is_file() for directory in (
            user / "build", Path("/usr/share/rime-data/build"),
        ))
    print("Installed CLI, engine manifests, and Rime data checks passed")


def check_removal():
    config = Path(pwd.getpwnam("omime-build").pw_dir) / ".config/oh-my-ime/config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    sentinel = "# Preserve this user's settings when uninstalling Rime.\n"
    config.write_text(sentinel)
    subprocess.run(["pacman", "-R", "--noconfirm", "oh-my-ime-rime-bin"], check=True)
    assert config.read_text() == sentinel
    assert not Path("/usr/bin/omime-rime").exists()
    assert not Path("/usr/share/oh-my-ime/engines/rime/engine.toml").exists()
    assert Path("/usr/share/oh-my-ime/engines/echo/engine.toml").is_file()
    assert Path("/usr/lib/systemd/user/omimed.service").is_file()
    assert Path("/usr/share/licenses/oh-my-ime-bin/LICENSE-MIT").is_file()
    subprocess.run(["omimed", "--version"], check=True)
    subprocess.run(["omime", "help"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["pacman", "-Qkk", "oh-my-ime-bin"], check=True)
    print("Rime removal preserves the platform package and user configuration")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    metadata = commands.add_parser("metadata")
    metadata.add_argument("expected")
    metadata.add_argument("actual")
    commands.add_parser("runtime")
    commands.add_parser("removal")
    arguments = parser.parse_args()
    if arguments.command == "metadata":
        check_metadata(arguments.expected, arguments.actual)
    elif arguments.command == "runtime":
        check_runtime()
    else:
        check_removal()
