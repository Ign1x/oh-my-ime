"""Build a native AppImage on Ubuntu 24.04 from verified release payloads.

The image uses per-ELF RUNPATHs. It never exports a library search path to hosted
input methods, and its launcher materializes a persistent tree before execution.
"""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tempfile
import urllib.request

from .common import PROGRAMS, SYSTEMS, sha256

TEMPLATES = Path(__file__).resolve().parents[2] / "packaging/appimage"
ALL_PROGRAMS = (*PROGRAMS, "omime-rime")
# glibc and the graphics driver stack must belong to the running host. The
# Wayland client/egl adapter is not a GPU driver and can be bundled normally.
HOST_LIBRARIES = re.compile(
    r"^(?:ld-linux.*|lib(?:c|m|dl|pthread|rt|resolv|util|anl|BrokenLocale|nss_[^.]+)\.so\..*"
    r"|lib(?:EGL|GL|GLX|GLdispatch|OpenGL|GLESv[12]|vulkan|drm[^.]*|gbm)\.so\..*)$"
)
# winit loads these with dlopen, so DT_NEEDED alone is insufficient.
DYNAMIC_LIBRARIES = ("libwayland-client.so.0", "libwayland-cursor.so.0", "libwayland-egl.so.1")
RIME_FILES = ("default.yaml", "double_pinyin_flypy.schema.yaml", "luna_pinyin.schema.yaml", "luna_pinyin.dict.yaml")


def command(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()


def check_elf(path, system):
    with Path(path).open("rb") as source:
        header = source.read(20)
    if (len(header) < 20 or header[:6] != b"\x7fELF\x02\x01"
            or struct.unpack_from("<H", header, 18)[0] != SYSTEMS[system]):
        raise ValueError(f"{path}: expected a native {system} ELF")


def baseline(system):
    if platform.system() != "Linux" or platform.machine() != system.removesuffix("-linux"):
        raise ValueError(f"AppImages must be built natively on {system}")
    os_release = platform.freedesktop_os_release()
    if (os_release.get("ID"), os_release.get("VERSION_ID")) != ("ubuntu", "24.04"):
        raise ValueError("build AppImages on Ubuntu 24.04 to preserve the glibc 2.39 baseline")
    for tool in ("patchelf", "mksquashfs", "readelf", "dpkg-query", "ldconfig"):
        if shutil.which(tool) is None:
            raise ValueError(f"required AppImage build tool is missing: {tool}")


def library_cache():
    libraries = {}
    for line in command("ldconfig", "-p").splitlines():
        match = re.match(r"\s*(\S+) \([^)]*\) => (\S+)", line)
        if match:
            libraries.setdefault(match[1], Path(match[2]))
    return libraries


def package_owner(path):
    path = Path(path)
    # Ubuntu's merged /usr layout has package lists using both spellings.
    candidates = [path, path.resolve()]
    for candidate in tuple(candidates):
        text = str(candidate)
        if text.startswith("/usr/lib/"):
            candidates.append(Path(text.removeprefix("/usr")))
        elif text.startswith("/lib/"):
            candidates.append(Path("/usr" + text))
    for candidate in dict.fromkeys(candidates):
        result = subprocess.run(["dpkg-query", "-S", str(candidate)], text=True, capture_output=True)
        if result.returncode == 0:
            owners = [line.split(": ", 1)[0] for line in result.stdout.splitlines() if ": " in line]
            if len(owners) == 1:
                return owners[0]
    raise ValueError(f"cannot identify the Ubuntu package and license for {path}")


def copy_package_license(package, appdir):
    name = package.split(":", 1)[0]
    source = Path("/usr/share/doc") / name / "copyright"
    if not source.is_file():
        raise ValueError(f"missing copyright file for bundled package {package}")
    destination = appdir / "usr/share/licenses/dependencies" / name
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination / "copyright")
    fields = command("dpkg-query", "-W", "-f=${Version}\t${source:Package}\t${source:Version}", package).split("\t")
    return {"package": package, "version": fields[0], "source_package": fields[1], "source_version": fields[2]}


def rime_prefix():
    value = os.environ.get("OMIME_APPIMAGE_RIME_PREFIX")
    if not value:
        raise ValueError("set OMIME_APPIMAGE_RIME_PREFIX to the prefix built by packaging/appimage/prepare-rime.py (librime >= 1.11 is required)")
    prefix = Path(value).resolve()
    record = json.loads((prefix / "rime-source.json").read_text())
    expected = json.loads((TEMPLATES / "rime-source.json").read_text())
    if record.get("source") != expected or sha256(prefix / "lib/librime.so.1") != record.get("library_sha256"):
        raise ValueError("the prepared librime prefix does not match its pinned source/build record")
    return prefix, record


def collect_libraries(appdir, system, prefix):
    libraries = library_cache()
    libraries["librime.so.1"] = prefix / "lib/librime.so.1"
    pending = list(DYNAMIC_LIBRARIES)
    binary_dir = appdir / "usr/lib/oh-my-ime/bin"
    for program in ALL_PROGRAMS:
        pending.extend(command("patchelf", "--print-needed", binary_dir / f"{program}.bin").splitlines())
    bundled = {}
    external = set()
    packages = set()
    destination = binary_dir.parent
    while pending:
        soname = pending.pop()
        if HOST_LIBRARIES.fullmatch(soname):
            external.add(soname)
            continue
        if soname in bundled:
            continue
        if "/" in soname or soname not in libraries:
            raise ValueError(f"required library is unavailable on the build host: {soname}")
        source = libraries[soname]
        check_elf(source, system)
        target = destination / soname
        shutil.copy2(source, target)
        if soname == "librime.so.1":
            package = "librime (pinned source build)"
        else:
            package = package_owner(source)
            packages.add(package)
        pending.extend(command("patchelf", "--print-needed", target).splitlines())
        subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", str(target)], check=True)
        bundled[soname] = {"package": package, "source_sha256": sha256(source), "bundled_sha256": sha256(target)}
    for program in ALL_PROGRAMS:
        subprocess.run(["patchelf", "--set-rpath", "$ORIGIN/..", str(binary_dir / f"{program}.bin")], check=True)
    # An accidental newer build host or local library must not silently increase
    # the advertised baseline. Version needs include every bundled transitive ELF.
    for path in [*(binary_dir / f"{p}.bin" for p in ALL_PROGRAMS), *(destination / name for name in bundled)]:
        versions = {tuple(map(int, m)) for m in re.findall(r"\bGLIBC_(\d+)\.(\d+)\b", command("readelf", "--version-info", path))}
        if versions and max(versions) > (2, 39):
            raise ValueError(f"{path.name} requires glibc {max(versions)}, above 2.39")
    return bundled, sorted(external), packages


def copy_data(source, destination):
    if not source.is_dir():
        raise ValueError(f"required runtime data is missing: {source}")
    # Dereference Ubuntu data links: absolute links back to the build host would
    # make dictionaries unavailable after relocation.
    shutil.copytree(source, destination, symlinks=False)
    packages = set()
    for directory, _, filenames in os.walk(source, followlinks=True):
        for name in filenames:
            path = Path(directory) / name
            if path.is_file():
                packages.add(package_owner(path.resolve()))
    return packages


def stage_payloads(payloads, appdir):
    binary_dir = appdir / "usr/lib/oh-my-ime/bin"
    binary_dir.mkdir(parents=True)
    (appdir / "usr/bin").mkdir(parents=True)
    launcher = (TEMPLATES / "launcher.sh").read_text()
    for component, names in (("platform", PROGRAMS), ("rime", ("omime-rime",))):
        payload = Path(payloads[component])
        shutil.copytree(payload / "share", appdir / "usr/share", dirs_exist_ok=True)
        for name in names:
            shutil.copy2(payload / "bin" / name, binary_dir / f"{name}.bin")
            wrapper = binary_dir / name
            wrapper.write_text(launcher.replace("@PROGRAM@", name))
            wrapper.chmod(0o755)
            (appdir / "usr/bin" / name).symlink_to(f"../lib/oh-my-ime/bin/{name}")
    shutil.copy2(TEMPLATES / "AppRun", appdir / "AppRun")
    (appdir / "AppRun").chmod(0o755)
    shutil.copy2(TEMPLATES / "oh-my-ime.svg", appdir / "oh-my-ime.svg")
    (appdir / ".DirIcon").symlink_to("oh-my-ime.svg")
    (appdir / "oh-my-ime.desktop").write_text(
        "[Desktop Entry]\nType=Application\nName=oh-my-ime Settings\n"
        "Exec=AppRun\nIcon=oh-my-ime\nTerminal=false\nCategories=Settings;\n"
    )


def tree_identity(appdir):
    digest = hashlib.sha256()
    for path in sorted(appdir.rglob("*")):
        relative = path.relative_to(appdir).as_posix()
        if path.is_symlink():
            description = [relative, "symlink", str(path.readlink())]
        elif path.is_file():
            description = [relative, "file", path.stat().st_mode & 0o777, sha256(path)]
        else:
            description = [relative, "directory"]
        digest.update(json.dumps(description, separators=(",", ":")).encode() + b"\n")
    return digest.hexdigest()


def runtime_file(system, work):
    pin = json.loads((TEMPLATES / "runtime.json").read_text())
    artifact = pin["artifacts"][system]
    configured = os.environ.get("OMIME_APPIMAGE_RUNTIME")
    destination = work / "runtime"
    if configured:
        shutil.copyfile(configured, destination)
    else:
        with urllib.request.urlopen(artifact["url"], timeout=120) as response, destination.open("wb") as output:
            shutil.copyfileobj(response, output)
    if sha256(destination) != artifact["sha256"]:
        raise ValueError("AppImage runtime SHA-256 mismatch; refusing to use an unverified tool")
    check_elf(destination, system)
    if destination.read_bytes()[8:11] != b"AI\x02":
        raise ValueError("the pinned runtime does not have the AppImage type 2 signature")
    return destination, {"version": pin["version"], "source_revision": pin["source_revision"], **artifact}


def build(release, system, payloads, out):
    baseline(system)
    prefix, rime = rime_prefix()
    out = Path(out)
    name = f"oh-my-ime-{release['version']}-{system.removesuffix('-linux')}"
    with tempfile.TemporaryDirectory(prefix="omime-appimage-") as directory:
        work = Path(directory)
        appdir = work / "AppDir"
        stage_payloads(payloads, appdir)
        bundled, external, packages = collect_libraries(appdir, system, prefix)
        for required in RIME_FILES:
            if not (Path("/usr/share/rime-data") / required).is_file():
                raise ValueError(f"missing Rime data {required}; install rime-data-double-pinyin and rime-data-luna-pinyin")
        packages |= copy_data(Path("/usr/share/rime-data"), appdir / "usr/share/rime-data")
        # Rime resolves OpenCC profiles relative to its shared data directory.
        packages |= copy_data(Path("/usr/share/opencc"), appdir / "usr/share/rime-data/opencc")
        packages |= copy_data(Path("/usr/share/X11/xkb"), appdir / "usr/share/X11/xkb")
        package_records = [copy_package_license(p, appdir) for p in sorted(packages)]
        license_dir = appdir / "usr/share/licenses"
        shutil.copytree("/usr/share/common-licenses", license_dir / "dependencies/common-licenses", symlinks=False)
        shutil.copytree(prefix / "share/licenses/librime", license_dir / "librime")
        runtime_license = license_dir / "appimage-runtime"
        runtime_license.mkdir()
        for path in sorted(TEMPLATES.glob("runtime-*.txt")):
            shutil.copy2(path, runtime_license / path.name)
        shutil.copy2(TEMPLATES / "README.md", appdir / "README-AppImage.md")
        runtime, runtime_record = runtime_file(system, work)
        metadata = {
            "version": release["version"], "system": system, "revision": release["revision"],
            "release_artifacts": release["artifacts"][system], "runtime": runtime_record,
            "glibc_minimum": "2.39", "host_libraries": external,
            "host_requirements": ["glibc >= 2.39", "supported Wayland compositor", "system fonts", "compatible graphics drivers for GPU rendering"],
            "bundled_libraries": dict(sorted(bundled.items())), "ubuntu_packages": package_records,
            "rime": rime,
            "tools": {tool: command(tool, flag).splitlines()[0] for tool, flag in (("patchelf", "--version"), ("mksquashfs", "-version"))},
        }
        (appdir / "build.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
        # Squashfs modes and the content ID do not depend on the builder's umask.
        for path in [appdir, *appdir.rglob("*")]:
            if not path.is_symlink():
                path.chmod(0o755 if path.is_dir() or os.access(path, os.X_OK) else 0o644)
        content_id = f"{release['version']}-{system}-{tree_identity(appdir)}"
        (appdir / ".omime-image-id").write_text(content_id + "\n")
        squashfs = work / "image.squashfs"
        subprocess.run([
            "mksquashfs", str(appdir), str(squashfs), "-noappend", "-all-root", "-no-xattrs",
            "-comp", "gzip", "-mkfs-time", "0", "-all-time", "0", "-processors", "2", "-no-progress",
        ], check=True)
        image = out / f"{name}.AppImage"
        assembled = work / image.name
        with assembled.open("wb") as output:
            for part in (runtime, squashfs):
                with part.open("rb") as source:
                    shutil.copyfileobj(source, output)
        assembled.chmod(0o755)
        metadata.update({"content_id": content_id, "file": image.name, "sha256": sha256(assembled)})
        record = out / f"{name}.AppImage.build.json"
        assembled_record = work / record.name
        assembled_record.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
        assembled_record.chmod(0o644)
        # Build all content before exposing output files to the release collector.
        shutil.copy2(assembled, image)
        shutil.copy2(assembled_record, record)
        return [image, record]
