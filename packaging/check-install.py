#!/usr/bin/env python3
"""Check an installed native package in a disposable container, without a desktop."""

import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib


PROGRAMS = ("omimed", "omime", "omime-ibus", "omime-panel", "omime-settings", "omime-echo", "omime-sparkle")
RUNTIME_LIBRARIES = (
    "libEGL.so.1", "libGL.so.1", "libGLESv2.so.2", "libfontconfig.so.1",
    "libfreetype.so.6", "libxkbcommon.so.0", "libxkbcommon-x11.so.0",
    "libwayland-client.so.0", "libwayland-cursor.so.0", "libwayland-egl.so.1",
    "libX11.so.6", "libX11-xcb.so.1", "libXcursor.so.1", "libXi.so.6",
    "libXrandr.so.2", "libvulkan.so.1",
)


def user_state():
    paths = [Path.home() / ".config", Path.home() / ".local/bin",
             Path.home() / ".local/share/oh-my-ime", Path("/etc/xdg/autostart"),
             Path("/etc/systemd/user/omimed.service")]
    for systemd_directory in (Path("/etc/systemd/user"), Path("/usr/lib/systemd/user")):
        paths.extend(systemd_directory.glob("*.wants/omimed.service"))
    result = {}
    for root in paths:
        if root.exists() or root.is_symlink():
            for path in [root, *sorted(root.rglob("*"))]:
                if path.is_symlink():
                    value = {"link": os.readlink(path)}
                elif path.is_file():
                    value = {"sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                else:
                    value = {"directory": True}
                result[str(path)] = value
    return result


def run(*command):
    return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT)


def owner(path, package):
    if shutil.which("dpkg-query"):
        result = run("dpkg-query", "-S", str(path))
        assert result.split(": ", 1)[0].split(":", 1)[0] == package, result
    else:
        assert run("rpm", "-qf", "--qf", "%{NAME}", str(path)) == package


def check_rime():
    """Use the same public API table as the engine to deploy and input Chinese."""
    library = ctypes.CDLL("librime.so.1")

    class Api(ctypes.Structure):
        # RimeApi through change_page (librime 1.11), matching the engine's ABI.
        _fields_ = [("data_size", ctypes.c_int), ("functions", ctypes.c_void_p * 98)]

    library.rime_get_api.restype = ctypes.POINTER(Api)
    api = library.rime_get_api().contents
    assert api.data_size >= ctypes.sizeof(Api) - ctypes.sizeof(ctypes.c_int), (
        f"librime API is too old for omime-rime: {api.data_size} bytes"
    )

    def function(index, result, *args):
        address = api.functions[index]
        assert address, f"missing librime API entry {index}"
        return ctypes.CFUNCTYPE(result, *args)(address)

    version = function(72, ctypes.c_char_p)().decode()
    assert tuple(int(part) for part in version.split(".")[:2]) >= (1, 11), version
    shared = Path("/usr/share/rime-data")
    for file in ("default.yaml", "double_pinyin_flypy.schema.yaml", "luna_pinyin.schema.yaml"):
        assert (shared / file).is_file(), f"missing Rime data: {file}"

    class Traits(ctypes.Structure):
        _fields_ = [("data_size", ctypes.c_int), ("shared_data_dir", ctypes.c_char_p),
                    ("user_data_dir", ctypes.c_char_p), ("distribution_name", ctypes.c_char_p),
                    ("distribution_code_name", ctypes.c_char_p), ("distribution_version", ctypes.c_char_p),
                    ("app_name", ctypes.c_char_p), ("modules", ctypes.c_void_p),
                    ("min_log_level", ctypes.c_int), ("log_dir", ctypes.c_char_p),
                    ("prebuilt_data_dir", ctypes.c_char_p), ("staging_dir", ctypes.c_char_p)]

    class Commit(ctypes.Structure):
        _fields_ = [("data_size", ctypes.c_int), ("text", ctypes.c_char_p)]

    with tempfile.TemporaryDirectory(prefix="omime-rime-smoke-") as temporary:
        (Path(temporary) / "default.custom.yaml").write_text(
            "patch:\n  schema_list:\n    - schema: double_pinyin_flypy\n    - schema: luna_pinyin\n"
        )
        traits = Traits(ctypes.sizeof(Traits) - ctypes.sizeof(ctypes.c_int),
                        str(shared).encode(), temporary.encode(), b"oh-my-ime package check",
                        b"oh-my-ime", b"1", b"rime.oh-my-ime.package-check", None, 2, b"", None, None)
        function(0, None, ctypes.POINTER(Traits))(ctypes.byref(traits))
        function(2, None, ctypes.POINTER(Traits))(ctypes.byref(traits))
        try:
            function(4, ctypes.c_int, ctypes.c_int)(1)
            function(6, None)()
            session = function(13, ctypes.c_size_t)()
            assert session, "Rime could not create a session"
            try:
                select = function(34, ctypes.c_int, ctypes.c_size_t, ctypes.c_char_p)
                assert select(session, b"double_pinyin_flypy"), "Xiaohe schema did not deploy"
                assert select(session, b"luna_pinyin"), "Pinyin schema did not deploy"
                process = function(18, ctypes.c_int, ctypes.c_size_t, ctypes.c_int, ctypes.c_int)
                for key in "nihao ":
                    process(session, ord(key), 0)
                committed = Commit(ctypes.sizeof(Commit) - ctypes.sizeof(ctypes.c_int), None)
                assert function(21, ctypes.c_int, ctypes.c_size_t, ctypes.POINTER(Commit))(
                    session, ctypes.byref(committed)), "Pinyin produced no committed text"
                try:
                    text = committed.text.decode()
                    assert any("\u4e00" <= character <= "\u9fff" for character in text), text
                    print(f"Rime {version}: both schemas deployed; nihao -> {text}")
                finally:
                    function(22, ctypes.c_int, ctypes.POINTER(Commit))(ctypes.byref(committed))
            finally:
                function(15, None, ctypes.c_size_t)(session)
        finally:
            function(3, None)()


def check(components):
    version = run("/usr/bin/omimed", "--version").strip()
    assert re.fullmatch(r"omimed \d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version), version
    run("/usr/bin/omime", "help")
    run("/usr/bin/omime-settings", "--help")
    for program in PROGRAMS + (("omime-rime",) if components == "all" else ()):
        path = Path("/usr/bin") / program
        owner(path, "oh-my-ime-rime" if program == "omime-rime" else "oh-my-ime")
        assert "not found" not in run("ldd", str(path)), program
    for library in RUNTIME_LIBRARIES:
        ctypes.CDLL(library)
    assert shutil.which("dbus-daemon"), "missing D-Bus daemon"
    assert run("fc-list", ":lang=zh", "-f", "%{file}\n").strip(), "missing Chinese font"
    unit = Path("/usr/lib/systemd/user/omimed.service")
    owner(unit, "oh-my-ime")
    assert "ExecStart=/usr/bin/omimed" in unit.read_text()
    assert "ExecReload=/usr/bin/omime reload" in unit.read_text()
    desktop = Path("/usr/share/applications/io.github.ign1x.OhMyIme.Settings.desktop")
    owner(desktop, "oh-my-ime")
    assert "Exec=/usr/bin/omime-settings" in desktop.read_text()
    for engine in ("echo", "sparkle", "ibus") + (("rime",) if components == "all" else ()):
        path = Path("/usr/share/oh-my-ime/engines") / engine / "engine.toml"
        manifest = tomllib.loads(path.read_text())["engine"]
        assert manifest["exec"][0] == f"/usr/bin/omime-{engine}", path
        if engine == "rime" and "settings" in manifest:
            assert manifest["settings"]["exec"][0] == f"/usr/bin/omime-{engine}", path
        owner(path, "oh-my-ime-rime" if engine == "rime" else "oh-my-ime")
    assert Path("/usr/share/oh-my-ime/config.toml").is_file()
    assert not Path("/etc/oh-my-ime").exists(), "installed a user config under /etc"
    if components == "all":
        check_rime()
    print(f"{version}: installed paths, ELF/dlopen libraries, fonts, CLI and {components} components passed")


if __name__ == "__main__":
    command, argument = sys.argv[1:]
    if command == "snapshot":
        Path(argument).write_text(json.dumps(user_state(), sort_keys=True))
    elif command == "unchanged":
        assert json.loads(Path(argument).read_text()) == user_state(), "installation changed user configuration"
    elif command == "check" and argument in {"platform", "all"}:
        check(argument)
    else:
        raise SystemExit("usage: check-install.py snapshot|unchanged FILE, or check platform|all")
