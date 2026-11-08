"""Validate release inputs and stage the shared Linux installation layout."""

import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import tarfile

SYSTEMS = {"x86_64-linux": 62, "aarch64-linux": 183}
PROGRAMS = (
    "omimed", "omime", "omime-ibus", "omime-panel", "omime-settings",
    "omime-echo", "omime-sparkle",
)


def load_release(path):
    """Accept an assembled release or one native build's manifest."""
    release = json.loads(Path(path).read_text())
    version = release.get("version", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid release version")
    if release.get("tag") != f"v{version}":
        raise ValueError("release tag and version disagree")
    if not re.fullmatch(r"[0-9a-f]{40}", release.get("revision", "")):
        raise ValueError("release revision must be a full commit SHA")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", release.get("repository", "")):
        raise ValueError("release repository must be OWNER/REPO")
    if "system" in release:
        system = release.pop("system")
        release["artifacts"] = {system: release["artifacts"]}
    if not release.get("artifacts") or not set(release["artifacts"]) <= set(SYSTEMS):
        raise ValueError("release contains an unsupported or missing architecture")
    for system, components in release["artifacts"].items():
        if set(components) != {"platform", "rime"}:
            raise ValueError(f"{system}: expected platform and Rime archives")
        for component, artifact in components.items():
            prefix = "oh-my-ime" if component == "platform" else "oh-my-ime-rime"
            expected = f"{prefix}-{version}-{system}.tar.gz"
            if artifact.get("file") != expected:
                raise ValueError(f"{system}: unexpected archive filename")
            if not re.fullmatch(r"[0-9a-f]{64}", artifact.get("sha256", "")):
                raise ValueError(f"{expected}: invalid SHA-256")
    return release


def sha256(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def extract_payload(release, system, component, artifacts, destination):
    """Verify the archive before extracting regular files under a single root."""
    artifact = release["artifacts"][system][component]
    archive = Path(artifacts) / artifact["file"]
    if sha256(archive) != artifact["sha256"]:
        raise ValueError(f"{archive.name}: checksum mismatch")
    root_name = artifact["file"].removesuffix(".tar.gz")
    root = Path(destination) / root_name
    seen = set()
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or not path.parts or path.parts[0] != root_name
                    or ".." in path.parts or "\\" in member.name
                    or path in seen or not (member.isdir() or member.isfile())):
                raise ValueError(f"{archive.name}: unsafe or duplicate archive member {member.name!r}")
            seen.add(path)
            target = Path(destination).joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as data, target.open("xb") as output:
                    shutil.copyfileobj(data, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
    programs = PROGRAMS if component == "platform" else ("omime-rime",)
    for name in programs:
        binary = root / "bin" / name
        with binary.open("rb") as stream:
            header = stream.read(20)
        if (len(header) != 20 or header[:6] != b"\x7fELF\x02\x01"
                or struct.unpack_from("<H", header, 18)[0] != SYSTEMS[system]
                or not os.access(binary, os.X_OK)):
            raise ValueError(f"{name}: expected an executable ELF for {system}")
    return root


def stage_component(payload, destination, component):
    """Install one verified payload below a package build root, never the host."""
    if component not in {"platform", "rime"}:
        raise ValueError("unknown release component")
    payload, destination = Path(payload), Path(destination)
    prefix = destination / "usr"
    shutil.copytree(payload / "bin", prefix / "bin")
    shutil.copytree(payload / "share", prefix / "share")
    package = "oh-my-ime" if component == "platform" else "oh-my-ime-rime"
    if component == "rime":
        (prefix / "share/licenses/oh-my-ime").rename(prefix / "share/licenses/oh-my-ime-rime")
    else:
        unit = prefix / "share/systemd/user/omimed.service"
        text = unit.read_text().replace("%h/.local/bin/omimed", "/usr/bin/omimed")
        text = text.replace("%h/.local/bin/omime reload", "/usr/bin/omime reload")
        target = prefix / "lib/systemd/user/omimed.service"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        shutil.rmtree(prefix / "share/systemd")
        desktop = prefix / "share/applications/io.github.ign1x.OhMyIme.Settings.desktop"
        desktop.write_text(desktop.read_text().replace("Exec=omime-settings", "Exec=/usr/bin/omime-settings"))
    for engine in ("echo", "sparkle", "rime", "ibus"):
        manifest = prefix / "share/oh-my-ime/engines" / engine / "engine.toml"
        if manifest.exists():
            text = manifest.read_text()
            text = text.replace(f'["omime-{engine}"', f'["/usr/bin/omime-{engine}"')
            manifest.write_text(text)
    docs = prefix / "share/doc" / package
    docs.mkdir(parents=True, exist_ok=True)
    shutil.copy2(payload / "README.txt", docs / "README.release")


def identity(release):
    return {key: release[key] for key in ("version", "tag", "revision", "repository")}


def file_record(path):
    path = Path(path)
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").digest()
    return {"file": path.name, "sha256": digest.hex(),
            "hash": "sha256-" + base64.b64encode(digest).decode()}


def collect_packages(release, directory):
    """Accept only all six native format records from this exact release."""
    directory = Path(directory)
    packages, seen = {}, set()
    for system in SYSTEMS:
        packages[system] = {}
        for kind, count, suffix in (("deb", 2, ".deb"), ("rpm", 2, ".rpm"),
                                    ("appimage", 2, ".AppImage")):
            record = json.loads((directory / f"linux-{system}-{kind}.json").read_text())
            if any(record.get(key) != value for key, value in identity(release).items()):
                raise ValueError(f"{system}/{kind}: package source identity mismatch")
            if record.get("system") != system or record.get("format") != kind:
                raise ValueError(f"{system}/{kind}: wrong package format or architecture")
            artifacts = record.get("artifacts", [])
            if len(artifacts) != count:
                raise ValueError(f"{system}/{kind}: incomplete package set")
            if kind == "appimage":
                names = {item.get("file", "") for item in artifacts}
                images = [name for name in names if name.endswith(".AppImage")]
                if len(images) != 1 or names != {images[0], images[0] + ".build.json"}:
                    raise ValueError(f"{system}: expected one AppImage and its build provenance")
            packages[system][kind] = []
            for artifact in artifacts:
                name = artifact.get("file", "")
                valid_suffix = name.endswith(suffix) or (kind == "appimage" and name.endswith(".AppImage.build.json"))
                if (not name.startswith("oh-my-ime") or not valid_suffix
                        or Path(name).name != name or "\\" in name or name in seen):
                    raise ValueError(f"invalid or duplicate Linux package filename: {name!r}")
                verified = file_record(directory / name)
                if any(artifact.get(key) != value for key, value in verified.items()):
                    raise ValueError(f"{name}: package checksum mismatch")
                seen.add(name)
                packages[system][kind].append(verified)
    return packages
