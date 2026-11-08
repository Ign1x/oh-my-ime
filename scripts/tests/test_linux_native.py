"""Checks for native package compatibility boundaries and version ordering."""

from pathlib import Path
import os
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from linux_packages import native


class NativePackagesTest(unittest.TestCase):
    def test_native_version_spelling_and_prerelease_ordering(self):
        self.assertEqual(native.package_version("deb", "0.5.0"), "0.5.0-1")
        self.assertEqual(native.package_version("deb", "0.5.1-rc.1"), "0.5.1~rc.1-1")
        self.assertEqual(native.package_version("rpm", "0.5.0"), ("0.5.0", "1"))
        self.assertEqual(native.package_version("rpm", "0.5.1-rc.1"), ("0.5.1", "0.1.rc.1"))
        self.assertEqual(native.package_version("rpm", "0.5.1-alpha-test.2"),
                         ("0.5.1", "0.1.alpha.test.2"))

    def test_invalid_versions_cannot_enter_package_metadata(self):
        for version in ("v0.5.0", "0.5.0\nRequires: bad", "0.5.0%{bad}", "0.5.0/1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                native.package_version("rpm", version)

    def test_cross_architecture_requires_a_native_runner(self):
        with patch.object(native.platform, "system", return_value="Linux"), \
             patch.object(native.platform, "machine", return_value="x86_64"), \
             self.assertRaisesRegex(ValueError, "native aarch64"):
            native._check_native("deb", "aarch64-linux", {})

    def test_foreign_userland_cannot_label_host_binaries(self):
        with patch.object(native.platform, "system", return_value="Linux"), \
             patch.object(native.platform, "machine", return_value="x86_64"), \
             patch.object(native.shutil, "which", return_value="/usr/bin/tool"), \
             patch.object(native, "_output", return_value="arm64"), \
             self.assertRaisesRegex(ValueError, "toolchain architecture"):
            native._check_native("deb", "x86_64-linux", {})

    def test_mislabeled_payload_is_rejected_before_building(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "bin").mkdir()
            header = bytearray(20)
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<H", header, 18, 183)
            (root / "bin/omimed").write_bytes(header)
            with patch.object(native.platform, "system", return_value="Linux"), \
                 patch.object(native.platform, "machine", return_value="x86_64"), \
                 patch.object(native.shutil, "which", return_value="/usr/bin/tool"), \
                 patch.object(native, "_output", return_value="amd64"), \
                 self.assertRaisesRegex(ValueError, "expected an ELF executable"):
                native._check_native("deb", "x86_64-linux", {"platform": root})

    def test_permissive_builder_umask_does_not_leak_into_packages(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            previous = os.umask(0)
            try:
                (root / "DEBIAN").mkdir()
                (root / "DEBIAN/control").write_text("Package: example\n")
                (root / "usr/bin").mkdir(parents=True)
                (root / "usr/bin/omime").write_bytes(b"binary")
            finally:
                os.umask(previous)
            native._normalize_tree(root)
            for path in root.rglob("*"):
                self.assertFalse(path.stat().st_mode & 0o022, str(path))
            self.assertTrue(os.access(root / "usr/bin/omime", os.X_OK))
            self.assertEqual((root / "DEBIAN").stat().st_mode & 0o777, 0o755)


if __name__ == "__main__":
    unittest.main()
