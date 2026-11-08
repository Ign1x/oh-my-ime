"""Exercise AppRun's process lifetime, installation, dispatch and rollback.

These tests use private mock programs. Actual ELF/Rime checks are performed in
the Ubuntu packaging container without touching the user's desktop session.
"""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from linux_packages import appimage


PROGRAM = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, subprocess, sys
name = pathlib.Path(sys.argv[0]).name.removesuffix('.bin')
if sys.argv[1:] == ['spawn-child']:
    child = pathlib.Path(sys.argv[0]).with_name('omime-settings')
    code = 'import subprocess,sys,time; time.sleep(0.3); subprocess.run([sys.argv[1], "late child"], check=True)'
    with open(os.environ['OMIME_TEST_OUTPUT'], 'w') as output:
        subprocess.Popen([sys.executable, '-c', code, str(child)], stdout=output, stderr=output, start_new_session=True)
    sys.exit(0)
if sys.argv[1:] == ['settings']:
    os.execv(str(pathlib.Path(sys.argv[0]).with_name('omime-settings')), ['omime-settings', 'from sibling'])
print(json.dumps({'program': name, 'argv': sys.argv[1:], 'executable': sys.argv[0],
 'daemon': shutil.which('omimed'), 'environment': {name: os.environ.get(name) for name in
 ['PATH', 'XDG_DATA_DIRS', 'OMIME_RIME_SHARED_DATA', 'XKB_CONFIG_ROOT', 'LD_LIBRARY_PATH', 'APPDIR', 'APPIMAGE']}}))
'''


class AppRunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="omime-appimage-test-")
        self.base = Path(self.temporary.name)
        self.addCleanup(self.temporary.cleanup)
        self.payloads = {}
        for component, names in (("platform", appimage.PROGRAMS), ("rime", ("omime-rime",))):
            payload = self.base / component
            (payload / "bin").mkdir(parents=True)
            (payload / "share").mkdir()
            for name in names:
                path = payload / "bin" / name
                path.write_text(PROGRAM)
                path.chmod(0o755)
            self.payloads[component] = payload
        self.image = self.make_image("0.5.0-x86_64-linux-first")
        self.home = self.base / "private home"
        self.data = self.home / "custom data"
        self.cache = self.home / "custom cache"
        self.env = {
            **os.environ, "HOME": str(self.home), "XDG_DATA_HOME": str(self.data),
            "XDG_CACHE_HOME": str(self.cache), "XDG_DATA_DIRS": "/host/share:/usr/share",
            "LD_LIBRARY_PATH": "/host/intentional/library-path",
            "APPDIR": "/tmp/.mount_omime-test", "APPIMAGE": "/tmp/original.AppImage",
        }
        self.env.pop("OMIME_RIME_SHARED_DATA", None)
        self.env.pop("XKB_CONFIG_ROOT", None)
        self.install_base = self.data / "oh-my-ime/appimage"
        self.launchers = self.home / ".local/bin"

    def make_image(self, identity):
        image = self.base / ("image " + identity)
        appimage.stage_payloads(self.payloads, image)
        (image / ".omime-image-id").write_text(identity + "\n")
        return image

    def run_image(self, *args, image=None, env=None):
        return subprocess.run([str((image or self.image) / "AppRun"), *args], env=env or self.env,
                              text=True, capture_output=True, timeout=10)

    def report(self, *args, **kwargs):
        result = self.run_image(*args, **kwargs)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_default_and_selectors_forward_exact_arguments(self):
        self.assertEqual(self.report()["program"], "omime-settings")
        cli = self.report("status", "argument with spaces", "$(literal)")
        self.assertEqual(cli["program"], "omime")
        self.assertEqual(cli["argv"], ["status", "argument with spaces", "$(literal)"])
        self.assertEqual(self.report("--daemon", "--help")["program"], "omimed")
        self.assertEqual(self.report("--tool", "omime-rime", "--version")["program"], "omime-rime")
        for args in (("--tool",), ("--tool", "unknown"), ("--tool", "../omime")):
            result = self.run_image(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("oh-my-ime AppImage:", result.stderr)

    def test_subprocess_survives_source_image_removal(self):
        output = self.base / "late-output.json"
        result = self.run_image("--daemon", "spawn-child", env={**self.env, "OMIME_TEST_OUTPUT": str(output)})
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.rmtree(self.image)
        for _ in range(100):
            if output.is_file() and output.stat().st_size:
                break
            time.sleep(0.02)
        report = json.loads(output.read_text())
        self.assertEqual(report["program"], "omime-settings")
        self.assertEqual(report["argv"], ["late child"])
        self.assertTrue(report["executable"].startswith(str(self.cache)))
        self.assertNotIn(".mount_", report["executable"])

    def test_environment_keeps_host_libraries_and_override_data(self):
        report = self.report("doctor")
        env = report["environment"]
        self.assertEqual(env["LD_LIBRARY_PATH"], self.env["LD_LIBRARY_PATH"])
        self.assertIsNone(env["APPDIR"])
        self.assertIsNone(env["APPIMAGE"])
        self.assertTrue(env["OMIME_RIME_SHARED_DATA"].startswith(str(self.cache)))
        self.assertTrue(env["XDG_DATA_DIRS"].endswith(":/host/share:/usr/share"))
        override = self.report("--tool", "omime-rime", env={**self.env, "OMIME_RIME_SHARED_DATA": "/my/rime-data", "XKB_CONFIG_ROOT": "/my/xkb"})
        self.assertEqual(override["environment"]["OMIME_RIME_SHARED_DATA"], "/my/rime-data")
        self.assertEqual(override["environment"]["XKB_CONFIG_ROOT"], "/my/xkb")

    def test_setup_requires_explicit_install_even_after_cache_creation(self):
        self.report("--version")
        for args in (("setup",), ("setup", "--dry-run"), ("--tool", "omime", "setup", "--start")):
            result = self.run_image(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--install", result.stderr)
        self.assertEqual(self.report("setup", "--help")["program"], "omime")
        self.assertFalse(self.install_base.exists())
        self.assertFalse(self.launchers.exists())

    def test_install_and_sibling_launch_work_after_original_is_gone(self):
        result = self.run_image("--install")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.cache.exists())
        shutil.rmtree(self.image)
        command = self.launchers / "omime"
        setup = subprocess.run([str(command), "setup", "--dry-run"], env=self.env, text=True, capture_output=True, timeout=10)
        self.assertEqual(setup.returncode, 0, setup.stderr)
        report = json.loads(setup.stdout)
        self.assertEqual(report["daemon"], str(self.install_base / "current/usr/bin/omimed"))
        self.assertNotIn(".mount_", setup.stdout)
        sibling = subprocess.run([str(command), "settings"], env=self.env, text=True, capture_output=True, timeout=10)
        self.assertEqual(sibling.returncode, 0, sibling.stderr)
        self.assertEqual(json.loads(sibling.stdout)["program"], "omime-settings")
        for name in appimage.ALL_PROGRAMS:
            self.assertTrue((self.launchers / name).is_file())

    def test_upgrade_switches_current_and_keeps_previous_tree(self):
        self.assertEqual(self.run_image("--install").returncode, 0)
        old = (self.install_base / "current").resolve()
        new_image = self.make_image("0.5.0-x86_64-linux-rebuilt")
        result = self.run_image("--install", image=new_image)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual((self.install_base / "current").resolve(), old)
        self.assertTrue((old / "AppRun").exists())
        self.assertFalse((self.install_base / ".install-lock").exists())

    def test_unrelated_launcher_is_preserved_and_install_stops_before_copy(self):
        self.launchers.mkdir(parents=True)
        existing = self.launchers / "omimed"
        existing.write_text("existing user program")
        result = self.run_image("--install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to replace", result.stderr)
        self.assertEqual(existing.read_text(), "existing user program")
        self.assertEqual(list(self.install_base.iterdir()), [])
        self.assertEqual(list(self.launchers.iterdir()), [existing])

    def test_failed_activation_cleans_new_tree_and_preserves_working_install(self):
        self.assertEqual(self.run_image("--install").returncode, 0)
        old = (self.install_base / "current").resolve()
        image = self.make_image("0.5.0-x86_64-linux-failed")
        fakebin = self.base / "failure-bin"
        fakebin.mkdir()
        wrapper = fakebin / "mv"
        wrapper.write_text("#!/bin/sh\n[ \"${1:-}\" != -Tf ] || exit 23\nexec " + shlex.quote(shutil.which("mv")) + " \"$@\"\n")
        wrapper.chmod(0o755)
        result = self.run_image("--install", image=image, env={**self.env, "PATH": f"{fakebin}:{self.env['PATH']}"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.install_base / "current").resolve(), old)
        self.assertEqual({p.name for p in self.install_base.iterdir()}, {"current", old.name})
        self.assertTrue(all((self.launchers / name).is_file() for name in appimage.ALL_PROGRAMS))

    def test_failed_copy_cleans_temporary_cache(self):
        fakebin = self.base / "copy-failure-bin"
        fakebin.mkdir()
        wrapper = fakebin / "cp"
        wrapper.write_text("#!/bin/sh\nexit 23\n")
        wrapper.chmod(0o755)
        result = self.run_image("--version", env={**self.env, "PATH": f"{fakebin}:{self.env['PATH']}"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list((self.cache / "oh-my-ime/appimage").iterdir()), [])

    def test_failure_after_current_rename_keeps_the_activated_tree(self):
        self.assertEqual(self.run_image("--install").returncode, 0)
        old = (self.install_base / "current").resolve()
        image = self.make_image("0.5.0-x86_64-linux-activated")
        fakebin = self.base / "post-rename-failure-bin"
        fakebin.mkdir()
        wrapper = fakebin / "mv"
        real_mv = shlex.quote(shutil.which("mv"))
        wrapper.write_text("#!/bin/sh\n" + real_mv + " \"$@\" || exit $?\n[ \"${1:-}\" != -Tf ] || exit 23\n")
        wrapper.chmod(0o755)
        result = self.run_image("--install", image=image, env={**self.env, "PATH": f"{fakebin}:{self.env['PATH']}"})
        self.assertNotEqual(result.returncode, 0)
        current = (self.install_base / "current").resolve()
        self.assertEqual(current.name, "0.5.0-x86_64-linux-activated")
        self.assertTrue((current / "AppRun").is_file())
        self.assertTrue((old / "AppRun").is_file())
        self.assertTrue(all((self.launchers / name).is_file() for name in appimage.ALL_PROGRAMS))
        self.assertEqual({p.name for p in self.install_base.iterdir()}, {"current", old.name, current.name})

    def test_relative_xdg_path_is_refused(self):
        result = self.run_image("--install", env={**self.env, "XDG_DATA_HOME": "relative"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("absolute path", result.stderr)

    def test_installation_cannot_target_a_temporary_appimage_mount(self):
        destination = self.base / ".mount_test/data"
        result = self.run_image("--install", env={**self.env, "XDG_DATA_HOME": str(destination)})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("temporary AppImage mount", result.stderr)
        self.assertFalse(destination.exists())


class BuildIdentityTests(unittest.TestCase):
    def test_host_graphics_dependencies_are_not_copied_into_the_image(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            appdir = root / "AppDir"
            binaries = appdir / "usr/lib/oh-my-ime/bin"
            binaries.mkdir(parents=True)
            for program in appimage.ALL_PROGRAMS:
                (binaries / f"{program}.bin").write_bytes(b"executable fixture")
            prefix = root / "rime"
            (prefix / "lib").mkdir(parents=True)
            (prefix / "lib/librime.so.1").write_bytes(b"\x7fELF\x02\x01" + bytes(12) + b"\x3e\x00")

            def output(*args):
                if args[:2] == ("patchelf", "--print-needed"):
                    if Path(args[2]).name == "librime.so.1":
                        return "libstdc++.so.6\nlibunwind.so.8\nlibz.so.1\nlibexpat.so.1\nlibfuture-graphics.so.1"
                    return "librime.so.1\nlibfontconfig.so.1\nlibgcc_s.so.1"
                return "GLIBC_2.39 GLIBCXX_3.4.32 CXXABI_1.3.13 GCC_4.2.0"

            with patch.object(appimage, "library_cache", return_value={}), patch.object(appimage, "command", side_effect=output), patch.object(appimage.subprocess, "run"):
                bundled, external, _, abi = appimage.collect_libraries(appdir, "x86_64-linux", prefix)
            self.assertEqual(set(bundled), {"librime.so.1"})
            for name in ("libstdc++.so.6", "libgcc_s.so.1", "libunwind.so.8", "libz.so.1", "libexpat.so.1",
                         "libfuture-graphics.so.1", "libfontconfig.so.1", "libwayland-client.so.0", "libEGL.so.1"):
                self.assertIn(name, external)
                self.assertFalse((binaries.parent / name).exists())
            self.assertEqual(abi["GLIBCXX"], "3.4.32")

            def too_new(*args):
                return output(*args) if args[0] != "readelf" else "GLIBCXX_3.4.33"

            with patch.object(appimage, "library_cache", return_value={}), patch.object(appimage, "command", side_effect=too_new), patch.object(appimage.subprocess, "run"):
                with self.assertRaisesRegex(ValueError, "GLIBCXX_3.4.33"):
                    appimage.collect_libraries(appdir, "x86_64-linux", prefix)

    def test_tree_identity_changes_when_same_version_libraries_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            library = root / "library.so.1"
            library.write_text("old")
            before = appimage.tree_identity(root)
            library.write_text("new")
            self.assertNotEqual(appimage.tree_identity(root), before)

    def test_configured_runtime_is_still_checksum_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            runtime = work / "unverified"
            runtime.write_bytes(b"untrusted executable")
            with patch.dict(os.environ, {"OMIME_APPIMAGE_RUNTIME": str(runtime)}):
                with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                    appimage.runtime_file("x86_64-linux", work)


if __name__ == "__main__":
    unittest.main()
