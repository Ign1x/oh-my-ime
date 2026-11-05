#!/usr/bin/env python3
"""Package release binaries and assemble the Nix flake with pinned asset hashes.

The build job runs `package` on each native Linux runner. After those archives pass
the installed Nix tests, `assemble` creates the binary-only flake and SHA256SUMS.
This script performs no network requests and never uploads or publishes a release.
"""

import argparse
import base64
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
SYSTEMS = {"x86_64-linux": 62, "aarch64-linux": 183}
PROGRAMS = ("omimed", "omime", "omime-ibus", "omime-panel", "omime-settings", "omime-echo", "omime-sparkle")


def version_for(tag):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", tag) or tag != f"v{version}":
        raise ValueError(f"release tag {tag!r} must equal Cargo.toml version v{version}")
    return version


def identity(args):
    version = version_for(args.tag)
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
        raise ValueError("repository must be OWNER/REPO")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    return {"version": version, "tag": args.tag, "revision": revision, "repository": args.repository}


def digests(path):
    with path.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").digest()
    return digest.hex(), "sha256-" + base64.b64encode(digest).decode()


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def archive(directory, destination):
    # Stable metadata, including gzip's header, gives the same hash on a rerun.
    with destination.open("wb") as output, gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as tar:
            for path in [directory, *sorted(directory.rglob("*"))]:
                member = tar.gettarinfo(path, arcname=str(path.relative_to(directory.parent)))
                member.uid = member.gid = member.mtime = 0
                member.uname = member.gname = ""
                member.mode = 0o755 if path.is_dir() or os.access(path, os.X_OK) else 0o644
                if path.is_file():
                    with path.open("rb") as data:
                        tar.addfile(member, data)
                else:
                    tar.addfile(member)


def check_elf(path, system):
    with path.open("rb") as source:
        header = source.read(20)
    if len(header) != 20 or header[:6] != b"\x7fELF\x02\x01":
        raise ValueError(f"{path} must be a 64-bit little-endian ELF binary")
    if struct.unpack_from("<H", header, 18)[0] != SYSTEMS[system]:
        raise ValueError(f"{path} has the wrong architecture for {system}")
    if not os.access(path, os.X_OK):
        raise ValueError(f"{path} is not executable")


def package(args):
    manifest = identity(args)
    daemon = (args.bin_dir / "omimed").resolve()
    check_elf(daemon, args.system)
    actual = subprocess.check_output([str(daemon), "--version"], text=True).strip()
    if actual != f"omimed {manifest['version']}":
        raise ValueError(f"built binary reports {actual!r}; rebuild version {manifest['version']}")
    manifest["system"] = args.system
    manifest["artifacts"] = {}
    args.out.mkdir(parents=True, exist_ok=True)
    for component, programs in (("platform", PROGRAMS), ("rime", ("omime-rime",))):
        prefix = "oh-my-ime" if component == "platform" else "oh-my-ime-rime"
        name = f"{prefix}-{manifest['version']}-{args.system}"
        with tempfile.TemporaryDirectory(prefix="omime-release-") as temporary:
            directory = Path(temporary) / name
            for program in programs:
                source = args.bin_dir / program
                check_elf(source, args.system)
                target = directory / "bin" / program
                copy(source, target)
                subprocess.run(["strip", "--strip-unneeded", str(target)], check=True)
            for engine in (("echo", "sparkle", "fcitx5", "ibus") if component == "platform" else ("rime",)):
                source = ROOT / ("data/engines" if engine in ("fcitx5", "ibus") else "engines") / engine / "engine.toml"
                copy(source, directory / "share/oh-my-ime/engines" / engine / "engine.toml")
            if component == "platform":
                copy(ROOT / "data/omimed.service", directory / "share/systemd/user/omimed.service")
                desktop = "io.github.ign1x.OhMyIme.Settings.desktop"
                copy(ROOT / "data" / desktop, directory / "share/applications" / desktop)
                copy(ROOT / "data/config.toml", directory / "share/oh-my-ime/config.toml")
                for snippet in sorted((ROOT / "data/hyprland").iterdir()):
                    copy(snippet, directory / "share/oh-my-ime/hyprland" / snippet.name)
            for license_file in ("LICENSE-MIT", "LICENSE-APACHE"):
                copy(ROOT / license_file, directory / "share/licenses/oh-my-ime" / license_file)
            copy(ROOT / "docs/release-binaries.txt", directory / "README.txt")
            target = args.out / f"{name}.tar.gz"
            archive(directory, target)
        checksum, sri = digests(target)
        manifest["artifacts"][component] = {"file": target.name, "sha256": checksum, "hash": sri}
    write_json(args.out / f"{args.system}.json", manifest)
    print(f"Packaged {args.system}: {', '.join(item['file'] for item in manifest['artifacts'].values())}")


def assemble(args):
    release = identity(args)
    release["artifacts"] = {}
    for system in args.systems:
        manifest = json.loads((args.out / f"{system}.json").read_text())
        for key in ("version", "tag", "revision", "repository"):
            if manifest.get(key) != release[key]:
                raise ValueError(f"{system}: mismatched {key}; rebuild all archives from the same release")
        if manifest.get("system") != system or set(manifest["artifacts"]) != {"platform", "rime"}:
            raise ValueError(f"{system}: incorrect artifact set")
        release["artifacts"][system] = {}
        for component, item in manifest["artifacts"].items():
            prefix = "oh-my-ime" if component == "platform" else "oh-my-ime-rime"
            expected = f"{prefix}-{release['version']}-{system}.tar.gz"
            if item["file"] != expected:
                raise ValueError(f"{system}: unexpected filename {item['file']!r}")
            checksum, sri = digests(args.out / expected)
            if (checksum, sri) != (item["sha256"], item["hash"]):
                raise ValueError(f"{expected}: checksum mismatch")
            release["artifacts"][system][component] = {
                **item,
                "url": f"https://github.com/{args.repository}/releases/download/{args.tag}/{expected}",
            }
    write_json(args.out / "release.json", release)
    with tempfile.TemporaryDirectory(prefix="omime-release-flake-") as temporary:
        directory = Path(temporary) / "oh-my-ime-nix"
        copy(ROOT / "nix/release-flake.nix", directory / "flake.nix")
        copy(ROOT / "flake.lock", directory / "flake.lock")
        for name in ("binary.nix", "nixos.nix", "home-manager.nix", "service-path.nix"):
            copy(ROOT / "nix" / name, directory / "nix" / name)
        copy(ROOT / "nix/release-package.nix", directory / "nix/package.nix")
        (directory / "nix/rime.nix").write_text(
            '{ callPackage }: callPackage ./package.nix { component = "rime"; }\n'
        )
        copy(args.out / "release.json", directory / "nix/release.json")
        copy(ROOT / "docs/releases.md", directory / "docs/releases.md")
        copy(ROOT / "docs/zh-CN/releases.md", directory / "docs/zh-CN/releases.md")
        (directory / "README.md").write_text(
            "# oh-my-ime release binaries\n\n"
            "[English](docs/releases.md) | [简体中文](docs/zh-CN/releases.md)\n"
        )
        for license_file in ("LICENSE-MIT", "LICENSE-APACHE"):
            copy(ROOT / license_file, directory / license_file)
        archive(directory, args.out / "oh-my-ime-nix.tar.gz")
    files = [args.out / "oh-my-ime-nix.tar.gz", args.out / "release.json"]
    files += [args.out / item["file"] for artifacts in release["artifacts"].values() for item in artifacts.values()]
    (args.out / "SHA256SUMS").write_text("".join(f"{digests(path)[0]}  {path.name}\n" for path in sorted(files)))
    print(f"Assembled release flake for {', '.join(args.systems)} with verified SHA-256 hashes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check-tag")
    check.add_argument("tag")
    for command in ("package", "assemble"):
        sub = commands.add_parser(command)
        sub.add_argument("--tag", required=True)
        sub.add_argument("--repository", default="Ign1x/oh-my-ime")
        sub.add_argument("--out", type=Path, required=True)
        if command == "package":
            sub.add_argument("--system", choices=SYSTEMS, required=True)
            sub.add_argument("--bin-dir", type=Path, required=True)
        else:
            sub.add_argument("--systems", choices=SYSTEMS, nargs="+", default=list(SYSTEMS))
    args = parser.parse_args()
    try:
        if args.command == "check-tag":
            print(version_for(args.tag))
        elif args.command == "package":
            package(args)
        else:
            assemble(args)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"release: {error}\n")


if __name__ == "__main__":
    main()
