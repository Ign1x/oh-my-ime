"""Artifact integrity checks for the release assembler; no network or builds."""

import argparse
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
SPEC = importlib.util.spec_from_file_location("release", Path(__file__).parents[1] / "release.py")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.out = self.root / "dist"
        self.out.mkdir()
        self.identity = {
            "version": "1.2.3",
            "tag": "v1.2.3",
            "revision": "a" * 40,
            "repository": "Ign1x/oh-my-ime",
        }
        self.args = argparse.Namespace(
            tag="v1.2.3", repository="Ign1x/oh-my-ime", out=self.out, systems=list(release.SYSTEMS)
        )

    def fixtures(self):
        for system in release.SYSTEMS:
            artifacts = {}
            for component in ("platform", "rime"):
                prefix = "oh-my-ime" if component == "platform" else "oh-my-ime-rime"
                name = f"{prefix}-1.2.3-{system}.tar.gz"
                directory = self.root / f"{system}-{component}"
                directory.mkdir()
                (directory / "payload").write_text(f"{system} {component}")
                release.archive(directory, self.out / name)
                checksum, sri = release.digests(self.out / name)
                artifacts[component] = {"file": name, "sha256": checksum, "hash": sri}
            release.write_json(self.out / f"{system}.json", {
                **self.identity, "system": system, "artifacts": artifacts,
            })

    def assemble(self):
        with patch.object(release, "identity", return_value=self.identity.copy()):
            release.assemble(self.args)

    def test_assembled_flake_uses_verified_assets_and_contains_both_modules(self):
        self.fixtures()
        self.assemble()
        manifest = json.loads((self.out / "release.json").read_text())
        for system, artifacts in manifest["artifacts"].items():
            for item in artifacts.values():
                self.assertIn(f"/releases/download/v1.2.3/{item['file']}", item["url"])
                self.assertEqual(release.digests(self.out / item["file"]), (item["sha256"], item["hash"]))
                self.assertIn(system, item["file"])
        with tarfile.open(self.out / "oh-my-ime-nix.tar.gz") as tar:
            names = set(tar.getnames())
            for name in ("flake.nix", "flake.lock", "nix/package.nix", "nix/rime.nix", "nix/binary.nix",
                         "nix/release.json", "nix/nixos.nix", "nix/home-manager.nix", "nix/service-path.nix"):
                self.assertIn(f"oh-my-ime-nix/{name}", names)
            self.assertEqual(json.load(tar.extractfile("oh-my-ime-nix/nix/release.json")), manifest)
        lines = (self.out / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(lines), 6)
        for line in lines:
            checksum, name = line.split("  ")
            self.assertEqual(checksum, release.digests(self.out / name)[0])
        first = (self.out / "oh-my-ime-nix.tar.gz").read_bytes()
        self.assemble()
        self.assertEqual(first, (self.out / "oh-my-ime-nix.tar.gz").read_bytes())

    def test_changed_archive_and_mixed_source_revisions_cannot_be_published(self):
        self.fixtures()
        manifest_path = self.out / "aarch64-linux.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["revision"] = "b" * 40
        release.write_json(manifest_path, manifest)
        with self.assertRaisesRegex(ValueError, "mismatched revision"):
            self.assemble()
        manifest["revision"] = self.identity["revision"]
        release.write_json(manifest_path, manifest)
        target = self.out / manifest["artifacts"]["rime"]["file"]
        target.write_bytes(target.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.assemble()
        self.assertFalse((self.out / "oh-my-ime-nix.tar.gz").exists())

    def test_incorrect_architecture_and_non_elf_executables_are_rejected(self):
        binary = self.root / "program"
        header = bytearray(20)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 62)
        binary.write_bytes(header)
        binary.chmod(0o755)
        release.check_elf(binary, "x86_64-linux")
        with self.assertRaisesRegex(ValueError, "wrong architecture"):
            release.check_elf(binary, "aarch64-linux")
        binary.write_text("#!/bin/sh\nexit 0\n")
        with self.assertRaisesRegex(ValueError, "ELF"):
            release.check_elf(binary, "x86_64-linux")

    def test_tags_must_match_the_workspace_version(self):
        with patch.object(release, "ROOT", self.root):
            (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
            self.assertEqual(release.version_for("v1.2.3"), "1.2.3")
            for tag in ("v1.2.4", "main", "v1.2.3/other", "1.2.3"):
                with self.assertRaises(ValueError):
                    release.version_for(tag)

    def test_packaging_rejects_a_stale_binary_version(self):
        args = argparse.Namespace(
            **vars(self.args), system="x86_64-linux", bin_dir=self.root
        )
        with patch.object(release, "identity", return_value=self.identity.copy()), \
                patch.object(release, "check_elf"), \
                patch.object(release.subprocess, "check_output", return_value="omimed 1.2.2\n"):
            with self.assertRaisesRegex(ValueError, "rebuild version 1.2.3"):
                release.package(args)
        self.assertEqual(list(self.out.iterdir()), [])

    def linux_fixtures(self):
        from linux_packages.common import file_record

        for system in release.SYSTEMS:
            for kind, suffix in (("deb", ".deb"), ("rpm", ".rpm"), ("appimage", ".AppImage")):
                if kind == "appimage":
                    names = [f"oh-my-ime-1.2.3-{system}.AppImage"]
                    names.append(names[0] + ".build.json")
                else:
                    names = [f"{prefix}-1.2.3-{system}{suffix}"
                             for prefix in ("oh-my-ime", "oh-my-ime-rime")]
                artifacts = []
                for name in names:
                    path = self.out / name
                    path.write_text(name)
                    artifacts.append(file_record(path))
                release.write_json(self.out / f"linux-{system}-{kind}.json", {
                    **self.identity, "system": system, "format": kind, "artifacts": artifacts,
                })

    def test_linux_packages_and_aur_are_covered_by_the_release_checksums(self):
        self.fixtures()
        self.linux_fixtures()
        self.args.linux_packages = True
        self.assemble()
        manifest = json.loads((self.out / "release.json").read_text())
        self.assertEqual(set(manifest["packages"]), set(release.SYSTEMS))
        names = {item["file"] for formats in manifest["packages"].values()
                 for artifacts in formats.values() for item in artifacts}
        names.add(manifest["aur"]["file"])
        checksums = {name: digest for digest, name in
                     (line.split("  ") for line in (self.out / "SHA256SUMS").read_text().splitlines())}
        self.assertEqual(len(checksums), 19)
        self.assertTrue(names <= checksums.keys())
        for name in names:
            self.assertEqual(checksums[name], release.digests(self.out / name)[0])
        with tarfile.open(self.out / manifest["aur"]["file"]) as archive:
            recipe = archive.extractfile("oh-my-ime-bin/PKGBUILD").read().decode()
        for system in release.SYSTEMS:
            for artifact in manifest["artifacts"][system].values():
                self.assertIn(artifact["sha256"], recipe)
        with tarfile.open(self.out / "oh-my-ime-nix.tar.gz") as archive:
            self.assertEqual(json.load(archive.extractfile("oh-my-ime-nix/nix/release.json")), manifest)

    def test_incomplete_linux_packages_cannot_produce_a_release_manifest(self):
        self.fixtures()
        self.linux_fixtures()
        self.args.linux_packages = True
        (self.out / "linux-aarch64-linux-appimage.json").unlink()
        with self.assertRaises(FileNotFoundError):
            self.assemble()
        self.assertFalse((self.out / "release.json").exists())
        self.assertFalse((self.out / "SHA256SUMS").exists())


if __name__ == "__main__":
    unittest.main()
