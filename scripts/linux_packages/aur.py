"""Generate an AUR split-package recipe pinned to both release architectures."""

import gzip
import io
from pathlib import Path
import re
import shlex
import tarfile


TEMPLATE = Path(__file__).resolve().parents[2] / "packaging/aur/PKGBUILD.in"
ARCHITECTURES = ("x86_64", "aarch64")
RUNTIME_DEPENDS = (
    "glibc>=2.39", "gcc-libs", "dbus", "fontconfig", "freetype2", "libglvnd",
    "libxkbcommon", "libx11", "libxcursor", "libxi", "libxrandr",
    "vulkan-icd-loader", "wayland",
)
RIME_DEPENDS = (
    "librime>=1.11.0", "rime-prelude", "rime-essay", "rime-luna-pinyin",
    "rime-double-pinyin",
)
PLATFORM_OPTIONAL = (
    "oh-my-ime-rime-bin: native Rime input engine",
    "fcitx5: host Fcitx 5 and its input methods",
    "fcitx5-configtool: configure hosted Fcitx 5",
    "ibus>=1.5.32: host IBus and its input methods",
    "systemd: user service autostart",
)


def package_version(version):
    """Keep prereleases older than stable in pacman's version comparison."""
    base, separator, prerelease = version.partition("-")
    # The alphabetic suffix must directly follow the base: an underscore here
    # would make the prerelease newer than stable. Prefix numeric labels too.
    return f"{base}pre{prerelease.replace('-', '_')}" if separator else base


def _metadata(release):
    """Keep shell input constrained even when this module is called directly."""
    version = release.get("version", "")
    repository = release.get("repository", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid AUR release version")
    if release.get("tag") != f"v{version}":
        raise ValueError("AUR release tag and version disagree")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid AUR release repository")
    if not re.fullmatch(r"[0-9a-f]{40}", release.get("revision", "")):
        raise ValueError("AUR release revision must be a full commit SHA")
    if set(release.get("artifacts", {})) != {f"{arch}-linux" for arch in ARCHITECTURES}:
        raise ValueError("AUR requires a full release manifest with x86_64 and aarch64")

    # Retain the upstream release spelling for URLs and extracted directories.
    pkgver = package_version(version)
    base = {
        "pkgbase": "oh-my-ime-bin",
        "pkgdesc": "Input method platform for Wayland desktops (release binaries)",
        "pkgver": pkgver,
        "pkgrel": "1",
        "url": f"https://github.com/{repository}",
        "arch": ARCHITECTURES,
        "license": ("MIT", "Apache-2.0"),
        "options": ("!strip", "!debug"),
    }
    for arch in ARCHITECTURES:
        components = release["artifacts"][f"{arch}-linux"]
        if set(components) != {"platform", "rime"}:
            raise ValueError(f"{arch}: AUR requires platform and Rime archives")
        sources, hashes = [], []
        for component, prefix in (("platform", "oh-my-ime"), ("rime", "oh-my-ime-rime")):
            artifact = components[component]
            filename = f"{prefix}-{version}-{arch}-linux.tar.gz"
            url = f"https://github.com/{repository}/releases/download/v{version}/{filename}"
            if artifact.get("file") != filename:
                raise ValueError(f"{arch}: unexpected AUR source filename")
            if artifact.get("url", url) != url:
                raise ValueError(f"{filename}: AUR source URL must match the release")
            if not re.fullmatch(r"[0-9a-f]{64}", artifact.get("sha256", "")):
                raise ValueError(f"{filename}: invalid AUR source SHA-256")
            sources.append(url)
            hashes.append(artifact["sha256"])
        base[f"source_{arch}"] = tuple(sources)
        base[f"sha256sums_{arch}"] = tuple(hashes)

    packages = {
        "oh-my-ime-bin": {
            "depends": RUNTIME_DEPENDS,
            "optdepends": PLATFORM_OPTIONAL,
            "provides": (f"oh-my-ime={pkgver}",),
            "conflicts": ("oh-my-ime",),
        },
        "oh-my-ime-rime-bin": {
            "pkgdesc": "Native Rime engine for oh-my-ime (release binaries)",
            "depends": (*RUNTIME_DEPENDS, f"oh-my-ime-bin={pkgver}-1", *RIME_DEPENDS),
            "provides": (f"oh-my-ime-rime={pkgver}",),
            "conflicts": ("oh-my-ime-rime",),
        },
    }
    return base, packages


def _shell_metadata(metadata, indent=""):
    lines = []
    for key, value in metadata.items():
        if isinstance(value, tuple):
            rendered = "(" + " ".join(shlex.quote(item) for item in value) + ")"
        else:
            rendered = shlex.quote(value)
        lines.append(f"{indent}{key}={rendered}")
    return "\n".join(lines)


def _srcinfo(base, packages):
    def fields(metadata):
        lines = []
        for key, values in metadata.items():
            for value in values if isinstance(values, tuple) else (values,):
                lines.append(f"\t{key} = {value}")
        return lines

    lines = [f"pkgbase = {base['pkgbase']}"]
    lines.extend(fields({key: value for key, value in base.items() if key != "pkgbase"}))
    for name, metadata in packages.items():
        lines.extend(("", f"pkgname = {name}"))
        lines.extend(fields(metadata))
    return "\n".join(lines) + "\n"


def build(release, out):
    """Return a deterministic source archive; never install or upload anything."""
    base, packages = _metadata(release)
    metadata = {"pkgbase": base["pkgbase"], "pkgname": tuple(packages),
                **{key: value for key, value in base.items() if key != "pkgbase"}}
    recipe = TEMPLATE.read_text()
    replacements = {
        "REVISION": release["revision"],
        "RELEASE_VERSION": release["version"],
        "METADATA": _shell_metadata(metadata),
        "PLATFORM_METADATA": _shell_metadata(packages["oh-my-ime-bin"], "  "),
        "RIME_METADATA": _shell_metadata(packages["oh-my-ime-rime-bin"], "  "),
    }
    for name, value in replacements.items():
        recipe = recipe.replace(f"@{name}@", value)
    contents = {"PKGBUILD": recipe, ".SRCINFO": _srcinfo(base, packages)}
    destination = Path(out) / f"oh-my-ime-aur-{release['version']}.tar.gz"
    with destination.open("wb") as stream, gzip.GzipFile(
        filename="", mode="wb", fileobj=stream, mtime=0,
    ) as compressed, tarfile.open(fileobj=compressed, mode="w") as archive:
        directory = tarfile.TarInfo("oh-my-ime-bin")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        archive.addfile(directory)
        for name, content in sorted(contents.items()):
            data = content.encode()
            member = tarfile.TarInfo(f"oh-my-ime-bin/{name}")
            member.mode = 0o644
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    return [destination]
