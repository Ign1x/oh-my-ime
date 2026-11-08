#!/usr/bin/env python3
"""Test a real AppImage in temporary HOME/XDG locations without desktop access."""

import ctypes as c
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile


def run(args, env, *, success=True, timeout=90):
    result = subprocess.run([str(arg) for arg in args], env=env, text=True, capture_output=True, timeout=timeout)
    if success and result.returncode:
        raise AssertionError(f"{args}: exit {result.returncode}\n{result.stdout}\n{result.stderr}")
    if "error while loading shared libraries" in result.stderr:
        raise AssertionError(result.stderr)
    return result


def rime_check(root, user):
    """Exercise the same public librime API as the engine, without a GUI/bus."""
    class Traits(c.Structure):
        _fields_ = [("data_size", c.c_int)] + [(name, c.c_char_p) for name in (
            "shared_data_dir", "user_data_dir", "distribution_name", "distribution_code_name",
            "distribution_version", "app_name")] + [
            ("modules", c.c_void_p), ("min_log_level", c.c_int), ("log_dir", c.c_char_p),
            ("prebuilt_data_dir", c.c_char_p), ("staging_dir", c.c_char_p)]

    class Composition(c.Structure):
        _fields_ = [(name, c.c_int) for name in ("length", "cursor_pos", "sel_start", "sel_end")] + [("preedit", c.c_char_p)]

    class Candidate(c.Structure):
        _fields_ = [("text", c.c_char_p), ("comment", c.c_char_p), ("reserved", c.c_void_p)]

    class Menu(c.Structure):
        _fields_ = [(name, c.c_int) for name in ("page_size", "page_no", "is_last_page", "highlighted_candidate_index", "num_candidates")] + [
            ("candidates", c.POINTER(Candidate)), ("select_keys", c.c_char_p)]

    class Context(c.Structure):
        _fields_ = [("data_size", c.c_int), ("composition", Composition), ("menu", Menu),
                    ("commit_text_preview", c.c_char_p), ("select_labels", c.c_void_p)]

    class Api(c.Structure):
        # Public ABI order in pinned librime 1.11.2: get_version is slot 72,
        # change_page is slot 97. All entries after data_size are pointers.
        _fields_ = [("data_size", c.c_int), ("functions", c.c_void_p * 98)]

    library = root / "usr/lib/oh-my-ime/librime.so.1"
    rime = c.CDLL(str(library))
    rime.rime_get_api.restype = c.c_void_p
    address = rime.rime_get_api()
    assert address and c.c_int.from_address(address).value >= c.sizeof(Api) - c.sizeof(c.c_int), "librime lacks change_page"
    api = Api.from_address(address)
    version = c.CFUNCTYPE(c.c_char_p)(api.functions[72])().decode()
    assert version == "1.11.2", version
    assert api.functions[97], "librime change_page is null"
    for name, result, args in (
        ("RimeSetup", None, [c.POINTER(Traits)]),
        ("RimeInitialize", None, [c.POINTER(Traits)]),
        ("RimeStartMaintenance", c.c_int, [c.c_int]),
        ("RimeJoinMaintenanceThread", None, []),
        ("RimeCreateSession", c.c_size_t, []),
        ("RimeDestroySession", c.c_int, [c.c_size_t]),
        ("RimeSelectSchema", c.c_int, [c.c_size_t, c.c_char_p]),
        ("RimeSetOption", None, [c.c_size_t, c.c_char_p, c.c_int]),
        ("RimeProcessKey", c.c_int, [c.c_size_t, c.c_int, c.c_int]),
        ("RimeGetContext", c.c_int, [c.c_size_t, c.POINTER(Context)]),
        ("RimeFreeContext", c.c_int, [c.POINTER(Context)]),
        ("RimeFinalize", None, []),
    ):
        function = getattr(rime, name)
        function.restype, function.argtypes = result, args
    user.mkdir(parents=True)
    # Force deployment from the relocated source dictionaries, rather than
    # depending only on distro-prebuilt binaries from another librime release.
    (user / "default.custom.yaml").write_text(
        "patch:\n  schema_list:\n    - schema: double_pinyin_flypy\n    - schema: luna_pinyin\n"
    )
    traits = Traits()
    traits.data_size = c.sizeof(Traits) - c.sizeof(c.c_int)
    traits.shared_data_dir = os.fsencode(root / "usr/share/rime-data")
    traits.user_data_dir = os.fsencode(user)
    traits.distribution_name = b"oh-my-ime AppImage smoke"
    traits.distribution_code_name = b"oh-my-ime"
    traits.distribution_version = b"0.5.0"
    traits.app_name = b"rime.oh-my-ime-appimage-smoke"
    traits.min_log_level = 2
    traits.log_dir = b""
    traits.prebuilt_data_dir = os.fsencode(user / "empty-prebuilt")
    (user / "empty-prebuilt").mkdir()
    rime.RimeSetup(c.byref(traits))
    rime.RimeInitialize(c.byref(traits))
    try:
        rime.RimeStartMaintenance(1)
        rime.RimeJoinMaintenanceThread()
        for schema, keys in ((b"double_pinyin_flypy", "hjyu"), (b"luna_pinyin", "hanyu")):
            session = rime.RimeCreateSession()
            assert session
            try:
                assert rime.RimeSelectSchema(session, schema), f"cannot select {schema!r}"
                rime.RimeSetOption(session, b"simplification", 1)
                for key in keys:
                    assert rime.RimeProcessKey(session, ord(key), 0), f"unhandled {key!r}"
                context = Context()
                context.data_size = c.sizeof(Context) - c.sizeof(c.c_int)
                assert rime.RimeGetContext(session, c.byref(context))
                try:
                    candidates = [context.menu.candidates[i].text.decode() for i in range(context.menu.num_candidates)]
                    assert "汉语" in candidates, (schema, candidates)
                finally:
                    rime.RimeFreeContext(c.byref(context))
                print(f"Rime {version}: {schema.decode()} -> 汉语 (bundled OpenCC)")
            finally:
                rime.RimeDestroySession(session)
    finally:
        rime.RimeFinalize()
    assert str(library) in Path("/proc/self/maps").read_text(), "librime was loaded from outside the bundle"


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--rime":
        rime_check(Path(sys.argv[2]), Path(sys.argv[3]))
        return
    if len(sys.argv) != 2:
        raise SystemExit("Usage: smoke.py OUTPUT_DIR")
    output = Path(sys.argv[1]).resolve()
    candidates = list(output.glob(f"oh-my-ime-*-{platform.machine()}.AppImage"))
    assert len(candidates) == 1, f"expected one native AppImage: {candidates}"
    artifact = candidates[0]
    record = json.loads(artifact.with_name(artifact.name + ".build.json").read_text())
    with artifact.open("rb") as source:
        assert hashlib.file_digest(source, "sha256").hexdigest() == record["sha256"]
    with tempfile.TemporaryDirectory(prefix="omime-appimage-smoke-") as temporary:
        work = Path(temporary)
        env = {key: value for key, value in os.environ.items() if key not in (
            "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "OMIME_ENGINE_PATH", "OMIME_RIME_SHARED_DATA",
            "XKB_CONFIG_ROOT", "LD_LIBRARY_PATH", "LD_PRELOAD", "APPDIR", "APPIMAGE")}
        env.update({
            "HOME": str(work / "home with spaces"), "XDG_CONFIG_HOME": str(work / "config"),
            "XDG_DATA_HOME": str(work / "data with spaces"), "XDG_CACHE_HOME": str(work / "cache"),
            "XDG_STATE_HOME": str(work / "state"), "XDG_RUNTIME_DIR": str(work / "runtime"),
            "XDG_DATA_DIRS": "/usr/local/share:/usr/share",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + str(work / "absent-bus"),
        })
        Path(env["XDG_RUNTIME_DIR"]).mkdir(mode=0o700)
        downloaded = work / "downloaded image.AppImage"
        shutil.copy2(artifact, downloaded)
        command = [downloaded, "--appimage-extract-and-run"]
        assert "Usage: omime <COMMAND>" in run([*command, "--help"], env).stdout
        assert run([*command, "--daemon", "--version"], env).stdout.strip() == f"omimed {record['version']}"
        assert run([*command, "--tool", "omime-settings", "--version"], env).stdout.strip() == f"omime-settings {record['version']}"
        refused = run([*command, "setup", "--dry-run"], env, success=False)
        assert refused.returncode and "--install" in refused.stderr
        install = run([*command, "--install"], env)
        assert "Installed oh-my-ime" in install.stdout
        current = Path(env["XDG_DATA_HOME"]) / "oh-my-ime/appimage/current"
        root = current.resolve()
        assert root.name == record["content_id"]
        assert (root / "build.json").is_file()
        # Launchers are independent of both the downloaded image and its cache.
        downloaded.unlink()
        shutil.rmtree(env["XDG_CACHE_HOME"])
        launchers = Path(env["HOME"]) / ".local/bin"
        assert "Usage: omime <COMMAND>" in run([launchers / "omime", "--help"], env).stdout
        assert run([launchers / "omimed", "--version"], env).stdout.strip() == f"omimed {record['version']}"
        assert run([launchers / "omime-settings", "--version"], env).stdout.strip() == f"omime-settings {record['version']}"
        config = Path(env["XDG_CONFIG_HOME"]) / "hypr/hyprland.conf"
        config.parent.mkdir(parents=True)
        config.write_text("# isolated dry-run configuration\n")
        preview = run([launchers / "omime", "setup", "--dry-run", "--engine", "rime", "--config", config], env)
        assert ".mount_" not in preview.stdout + preview.stderr
        assert config.read_text() == "# isolated dry-run configuration\n"
        assert not Path(env["XDG_STATE_HOME"]).exists(), "dry run wrote user state"
        for program in ("omimed", "omime", "omime-ibus", "omime-panel", "omime-settings", "omime-echo", "omime-sparkle", "omime-rime"):
            assert (launchers / program).resolve().is_file()
        for name in record["bundled_libraries"]:
            assert (root / "usr/lib/oh-my-ime" / name).is_file()
        assert (root / "usr/share/licenses/librime/LICENSE").is_file()
        assert (root / "usr/share/licenses/appimage-runtime/runtime-LICENSE.txt").is_file()
        for component in ("libfuse", "mimalloc", "musl", "squashfuse", "zlib", "zstd"):
            assert (root / f"usr/share/licenses/appimage-runtime/runtime-{component}-LICENSE.txt").is_file()
        result = run([sys.executable, Path(__file__).resolve(), "--rime", root, work / "rime-user"], env, timeout=180)
        print(result.stdout, end="")
        print("AppImage: verified native CLI, FUSEless launch, explicit install, source removal, setup preview, Rime deployment and conversion")


if __name__ == "__main__":
    main()
