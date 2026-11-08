#!/usr/bin/env python3
"""Render real AppImage windows with host Mesa/LLVM on a private Weston session.

Usage: graphics-smoke.py OUTPUT_DIR LOG_DIR
Run after prepare-runtime.sh --graphics-test, preferably as an unprivileged UID.
All input-method processes and files belong to this temporary test session.
"""

import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time


def descendants(pid):
    found = [pid]
    for parent in found:
        try:
            children = Path(f"/proc/{parent}/task/{parent}/children").read_text().split()
        except FileNotFoundError:
            continue
        found.extend(int(child) for child in children)
    return found


def process_maps(parent, program):
    for pid in descendants(parent):
        try:
            executable = os.readlink(f"/proc/{pid}/exe")
            if executable.endswith(f"/{program}.bin"):
                return Path(f"/proc/{pid}/maps").read_text()
        except FileNotFoundError:
            pass
    return ""


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def check_mappings(maps, bundle):
    paths = sorted({line.split()[-1] for line in maps.splitlines() if "/" in line})
    required = {
        "host EGL": r"/libEGL[^/]*\.so",
        "host Mesa driver": r"/(?:[^/]*dri\.so|libgallium[^/]*\.so)",
        "host LLVM": r"/libLLVM[^/]*\.so",
        "host C++ runtime": r"/libstdc\+\+\.so",
        "host GCC runtime": r"/libgcc_s(?:-[^/]*)?\.so",
        "host Wayland client": r"/libwayland-client\.so",
    }
    loaded = {}
    for requirement, expression in required.items():
        matched = [path for path in paths if re.search(expression, path)]
        assert matched, f"{requirement} was not loaded"
        assert all(bundle not in path for path in matched), f"{requirement} came from the AppImage: {matched}"
        loaded[requirement] = matched
    # Private Rime libraries may be inside the image. Every other shared object
    # must be from the host, including indirect graphics/runtime dependencies.
    allowed = {"librime.so.1", "libgflags.so.2.2", "libglog.so.1", "libleveldb.so.1d",
               "libmarisa.so.0", "libopencc.so.1.1", "libsnappy.so.1", "libyaml-cpp.so.0.8"}
    bundled = [path for path in paths if bundle in path and ".so" in Path(path).name]
    assert all(Path(path).name in allowed for path in bundled), bundled
    return loaded


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    output, logs = (Path(value).resolve() for value in sys.argv[1:])
    images = list(output.glob(f"oh-my-ime-*-{platform.machine()}.AppImage"))
    assert len(images) == 1, images
    artifact = images[0]
    record = json.loads(artifact.with_name(artifact.name + ".build.json").read_text())
    logs.mkdir(parents=True, exist_ok=True)
    summary = {"image_sha256": record["sha256"], "system": platform.freedesktop_os_release(),
               "renderer": "winit-femtovg", "mesa_driver": "llvmpipe", "windows": {}}
    with tempfile.TemporaryDirectory(prefix="omime-private-egl-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        (root / "tmp").mkdir(mode=0o700)
        env = {key: value for key, value in os.environ.items() if key not in (
            "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS",
            "LD_LIBRARY_PATH", "LD_PRELOAD", "OMIME_ENGINE_PATH", "OMIME_RIME_SHARED_DATA",
            "XKB_CONFIG_ROOT", "APPDIR", "APPIMAGE", "EGL_PLATFORM", "MESA_LOADER_DRIVER_OVERRIDE")}
        env.update(HOME=str(root / "home"), XDG_RUNTIME_DIR=str(runtime), XDG_DATA_HOME=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"), XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                   TMPDIR=str(root / "tmp"), XDG_DATA_DIRS="/usr/local/share:/usr/share",
                   LIBGL_ALWAYS_SOFTWARE="1", GALLIUM_DRIVER="llvmpipe")
        processes = []
        try:
            bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address"], env=env,
                                   text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
            processes.append(bus)
            env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
            assert env["DBUS_SESSION_BUS_ADDRESS"].startswith("unix:")
            weston = subprocess.Popen([
                "weston", "--backend=headless", "--renderer=gl", "--socket=private-wayland",
                "--idle-time=0", "--no-config", "--log=" + str(logs / "weston.log"),
            ], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True)
            processes.append(weston)
            for _ in range(300):
                assert weston.poll() is None, weston.stderr.read().decode()
                if (runtime / "private-wayland").exists():
                    break
                time.sleep(0.05)
            assert (runtime / "private-wayland").exists()
            assert "llvmpipe" in (logs / "weston.log").read_text(), "Weston did not select llvmpipe"
            env.update(WAYLAND_DISPLAY="private-wayland", SLINT_BACKEND="winit-femtovg", WAYLAND_DEBUG="client", EGL_PLATFORM="wayland")
            for label, program, arguments in (
                ("settings", "omime-settings", []),
                ("rime-settings", "omime-rime", ["--tool", "omime-rime", "--settings"]),
            ):
                logfile = logs / f"{label}.log"
                with logfile.open("w") as trace:
                    window = subprocess.Popen([str(artifact), "--appimage-extract-and-run", *arguments], env=env,
                                              stdout=trace, stderr=trace, start_new_session=True)
                    processes.append(window)
                    maps = ""
                    for _ in range(300):
                        assert window.poll() is None, logfile.read_text()
                        contents = logfile.read_text()
                        if re.search(r"wl_surface[@#]\d+\.attach\(wl_buffer[@#]", contents) and ".commit(" in contents:
                            maps = process_maps(window.pid, program)
                            if "libEGL" in maps and "libLLVM" in maps:
                                break
                        time.sleep(0.1)
                    assert maps, f"{label} did not render using EGL/LLVM: {logfile.read_text()}"
                    (logs / f"{label}.maps").write_text(maps)
                    assert "error while loading shared libraries" not in contents
                    assert "not found (required by" not in contents
                    summary["windows"][label] = check_mappings(maps, record["content_id"])
                    if program == "omime-rime":
                        assert record["content_id"] + "/usr/lib/oh-my-ime/librime.so.1" in maps
                    print(f"{label}: rendered with winit-femtovg and host EGL/Mesa/LLVM/C++/Wayland")
                    stop(window)
        finally:
            for process in reversed(processes):
                stop(process)
    (logs / "result.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
