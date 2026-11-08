"""Build native Debian and RPM packages from verified release payloads.

Run this module on the target architecture, inside the distribution used to
resolve dependencies. ELF dependencies come from the distribution's own scanner;
libraries opened by the UI at runtime are declared separately below.
"""

import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tempfile

from .common import PROGRAMS, SYSTEMS, stage_component


ARCHITECTURES = {
    "x86_64-linux": {"machine": "x86_64", "deb": "amd64", "rpm": "x86_64"},
    "aarch64-linux": {"machine": "aarch64", "deb": "arm64", "rpm": "aarch64"},
}

# Slint/winit/glutin/fontique load these at runtime, outside DT_NEEDED. Keep the
# X11 fallback and Wayland paths available, including EGL and system fonts.
DEB_UI_DEPENDS = (
    "libegl1", "libgl1", "libgles2", "libfontconfig1", "libfreetype6",
    "libxkbcommon0", "libxkbcommon-x11-0", "libwayland-client0",
    "libwayland-cursor0", "libwayland-egl1", "libx11-6", "libx11-xcb1",
    "libxcursor1", "libxi6", "libxrandr2", "libvulkan1",
)
RPM_UI_DEPENDS = tuple(f"{library}()(64bit)" for library in (
    "libEGL.so.1", "libGL.so.1", "libGLESv2.so.2", "libfontconfig.so.1",
    "libfreetype.so.6", "libxkbcommon.so.0", "libxkbcommon-x11.so.0",
    "libwayland-client.so.0", "libwayland-cursor.so.0", "libwayland-egl.so.1",
    "libX11.so.6", "libX11-xcb.so.1", "libXcursor.so.1", "libXi.so.6",
    "libXrandr.so.2", "libvulkan.so.1",
))


def package_version(kind, upstream):
    """Return native versions whose prereleases sort before their final release."""
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-([0-9A-Za-z.-]+))?", upstream)
    if not match:
        raise ValueError(f"invalid upstream version: {upstream!r}")
    version, prerelease = match.groups()
    if kind == "deb":
        return f"{version}{'~' + prerelease if prerelease else ''}-1"
    if kind == "rpm":
        revision = "0.1." + prerelease.replace("-", ".") if prerelease else "1"
        return version, revision
    raise ValueError(f"unsupported native package format: {kind}")


def _environment():
    return {**os.environ, "LC_ALL": "C", "SOURCE_DATE_EPOCH": os.environ.get("SOURCE_DATE_EPOCH", "0")}


def _output(command, **kwargs):
    return subprocess.check_output(command, text=True, env=_environment(), **kwargs).strip()


def _check_native(kind, system, payloads):
    if kind not in {"deb", "rpm"} or system not in ARCHITECTURES:
        raise ValueError(f"unsupported native target: {kind}/{system}")
    expected = ARCHITECTURES[system]
    if platform.system() != "Linux" or platform.machine() != expected["machine"]:
        raise ValueError(f"build {system} packages on a native {expected['machine']} Linux runner")
    commands = ("dpkg", "dpkg-deb", "dpkg-shlibdeps") if kind == "deb" else ("rpm", "rpmbuild")
    for command in commands:
        if not shutil.which(command):
            raise ValueError(f"{command} is required; use the {kind} packaging container")
    actual = _output(["dpkg", "--print-architecture"] if kind == "deb" else ["rpm", "--eval", "%{_arch}"])
    if actual != expected[kind]:
        raise ValueError(f"{kind} toolchain architecture {actual!r} does not match {system}")
    for component, programs in (("platform", PROGRAMS), ("rime", ("omime-rime",))):
        for name in programs:
            binary = Path(payloads[component]) / "bin" / name
            with binary.open("rb") as source:
                header = source.read(20)
            if (len(header) != 20 or header[:6] != b"\x7fELF\x02\x01"
                    or struct.unpack_from("<H", header, 18)[0] != SYSTEMS[system]):
                raise ValueError(f"{name}: expected an ELF executable for {system}")


def _description(component):
    return ("Modern Wayland-first input method platform" if component == "platform"
            else "Rime engine for the oh-my-ime input method platform")


def _name(component):
    return "oh-my-ime" if component == "platform" else "oh-my-ime-rime"


def _normalize_tree(root):
    # Package contents must not inherit a builder's permissive umask or user IDs.
    epoch = int(_environment()["SOURCE_DATE_EPOCH"])
    for path in [root, *root.rglob("*")]:
        executable = path.is_file() and path.parent == root / "usr/bin"
        path.chmod(0o755 if path.is_dir() or executable else 0o644)
        os.utime(path, (epoch, epoch))


def _deb_dependencies(root, component, version, scratch):
    # dpkg-shlibdeps needs a source control file even when emitting to stdout.
    debian = scratch / "debian"
    debian.mkdir(parents=True, exist_ok=True)
    (debian / "control").write_text(
        "Source: oh-my-ime\nSection: utils\nPriority: optional\n"
        "Maintainer: oh-my-ime contributors <noreply@github.com>\n\n"
        f"Package: {_name(component)}\nArchitecture: any\nDescription: {_description(component)}\n"
    )
    programs = PROGRAMS if component == "platform" else ("omime-rime",)
    generated = _output(
        ["dpkg-shlibdeps", "-O", *[f"-e{root.relative_to(scratch) / 'usr/bin' / name}" for name in programs]],
        cwd=scratch,
    )
    values = [line.removeprefix("shlibs:Depends=") for line in generated.splitlines()
              if line.startswith("shlibs:Depends=")]
    if len(values) != 1 or not values[0]:
        raise ValueError("dpkg-shlibdeps did not produce ELF dependencies")
    dependencies = values[0].split(", ")
    generated_names = {dependency.split(" ", 1)[0] for dependency in dependencies}
    dependencies += [dependency for dependency in DEB_UI_DEPENDS if dependency not in generated_names]
    if component == "platform":
        dependencies += ["dbus-daemon", "fontconfig", "fonts-noto-cjk"]
    else:
        # SONAME alone is insufficient: change_page was added in librime 1.11.
        dependencies += [f"oh-my-ime (= {version})", "librime1t64 (>= 1.11.0) | librime1 (>= 1.11.0)",
                         "librime-data", "rime-data-double-pinyin", "rime-data-luna-pinyin"]
    return ", ".join(sorted(set(dependencies)))


def _copyright(root, component):
    name = _name(component)
    licenses = root / "usr/share/licenses" / name
    text = (
        "oh-my-ime\nhttps://github.com/Ign1x/oh-my-ime\n\n"
        "Copyright: oh-my-ime contributors\n"
        "The project is available under the MIT or Apache-2.0 license.\n\n"
    )
    for path in sorted(licenses.iterdir()):
        if path.is_file():
            text += f"{path.name}\n\n{path.read_text()}\n"
    (root / "usr/share/doc" / name / "copyright").write_text(text)


def _deb(release, system, payloads, out, work):
    version = package_version("deb", release["version"])
    arch = ARCHITECTURES[system]["deb"]
    results = []
    for component in ("platform", "rime"):
        name = _name(component)
        scratch = work / component
        root = scratch / "debian" / name
        stage_component(payloads[component], root, component)
        _copyright(root, component)
        control = root / "DEBIAN"
        control.mkdir()
        depends = _deb_dependencies(root, component, version, scratch)
        files = sorted(path for path in root.rglob("*") if path.is_file())
        installed_size = sum((path.stat().st_size + 1023) // 1024 for path in files)
        (control / "control").write_text(
            f"Package: {name}\nVersion: {version}\nArchitecture: {arch}\n"
            "Section: utils\nPriority: optional\n"
            "Maintainer: oh-my-ime contributors <noreply@github.com>\n"
            f"Homepage: https://github.com/{release['repository']}\n"
            f"Installed-Size: {installed_size}\nDepends: {depends}\n"
            f"Description: {_description(component)}\n"
            " Packaged release binaries. User setup is performed explicitly with omime.\n"
        )
        (control / "md5sums").write_text("".join(
            f"{hashlib.md5(path.read_bytes(), usedforsecurity=False).hexdigest()}  {path.relative_to(root)}\n"
            for path in files
        ))
        _normalize_tree(root)
        target = out / f"{name}_{version}_{arch}.deb"
        subprocess.run(["dpkg-deb", "--root-owner-group", "-Zxz", "--uniform-compression",
                        "--build", str(root), str(target)], check=True, env=_environment())
        results.append(target)
    return results


def _rpm_files(root):
    lines = ["%defattr(-,root,root,-)"]
    for path in sorted(root.rglob("*")):
        relative = "/" + path.relative_to(root).as_posix()
        if path.is_dir():
            # Own application directories but not shared distro directories.
            if any(relative.startswith(prefix + "/") for prefix in (
                    "/usr/share/oh-my-ime", "/usr/share/doc", "/usr/share/licenses")):
                lines.append(f"%dir {relative}")
            elif relative == "/usr/share/oh-my-ime":
                lines.append(f"%dir {relative}")
        elif path.is_file():
            prefix = "%license " if relative.startswith("/usr/share/licenses/") else ""
            if relative.startswith("/usr/share/doc/"):
                prefix = "%doc "
            lines.append(prefix + relative)
    return "\n".join(lines)


def _rpm(release, system, payloads, out, work):
    version, revision = package_version("rpm", release["version"])
    arch = ARCHITECTURES[system]["rpm"]
    results = []
    for component in ("platform", "rime"):
        name = _name(component)
        top = work / name
        for directory in ("BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"):
            (top / directory).mkdir(parents=True)
        root = top / "SOURCES/payload"
        stage_component(payloads[component], root, component)
        _normalize_tree(root)
        dependencies = list(RPM_UI_DEPENDS)
        if component == "platform":
            dependencies += ["dbus-daemon", "fontconfig", "google-noto-sans-cjk-vf-fonts"]
        else:
            dependencies += [f"oh-my-ime%{{?_isa}} = {version}-{revision}",
                             "librime%{?_isa} >= 1.11.0", "brise"]
        spec = top / "SPECS" / f"{name}.spec"
        spec.write_text(
            # Released binaries are already stripped. Preserve them verbatim;
            # RPM's separate ELF dependency generator remains enabled.
            "%global debug_package %{nil}\n%global __os_install_post %{nil}\n"
            "%global _build_id_links none\n"
            f"Name: {name}\nVersion: {version}\nRelease: {revision}\n"
            f"Summary: {_description(component)}\nLicense: MIT OR Apache-2.0\n"
            f"URL: https://github.com/{release['repository']}\n"
            f"BuildArch: {arch}\nAutoReqProv: yes\n"
            + "".join(f"Requires: {dependency}\n" for dependency in dependencies)
            + f"\n%description\n{_description(component)}.\n"
            "User setup is performed explicitly with omime.\n"
            "\n%prep\n\n%build\n\n%install\n"
            'mkdir -p "%{buildroot}"\ncp -a "%{_sourcedir}/payload/." "%{buildroot}/"\n'
            "\n%files\n" + _rpm_files(root) + "\n"
        )
        subprocess.run([
            "rpmbuild", "-bb", "--target", arch, "--define", f"_topdir {top}",
            "--define", "_buildhost reproducible", "--define", "use_source_date_epoch_as_buildtime 1",
            "--define", "clamp_mtime_to_source_date_epoch 1", str(spec),
        ], check=True, env=_environment())
        packages = list((top / "RPMS").rglob("*.rpm"))
        if len(packages) != 1:
            raise ValueError(f"expected one {name} RPM, found {len(packages)}")
        target = out / packages[0].name
        shutil.copyfile(packages[0], target)
        results.append(target)
    return results


def build(kind, release, system, payloads, out):
    """Build both split packages with native ELF dependency metadata."""
    _check_native(kind, system, payloads)
    out = Path(out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="omime-native-") as temporary:
        builder = _deb if kind == "deb" else _rpm
        return builder(release, system, payloads, out, Path(temporary))
