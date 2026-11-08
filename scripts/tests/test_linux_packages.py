"""Reject corrupt distribution inputs and keep split packages independently owned."""

import hashlib
import io
import json
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parents[1]))
from linux_packages import common


class LinuxPackageInputs(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.release = {
            "version": "0.5.0", "tag": "v0.5.0", "revision": "a" * 40,
            "repository": "Ign1x/oh-my-ime", "artifacts": {},
        }
        for system in common.SYSTEMS:
            self.release["artifacts"][system] = {
                component: {
                    "file": f"{prefix}-0.5.0-{system}.tar.gz", "sha256": "b" * 64,
                }
                for component, prefix in (("platform", "oh-my-ime"), ("rime", "oh-my-ime-rime"))
            }

    def archive(self, members):
        item = self.release["artifacts"]["x86_64-linux"]["rime"]
        archive = self.root / item["file"]
        with tarfile.open(archive, "w:gz") as tar:
            for name, data, kind in members:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.mode = 0o755
                if kind == tarfile.REGTYPE:
                    member.size = len(data)
                    tar.addfile(member, io.BytesIO(data))
                else:
                    member.linkname = data.decode()
                    tar.addfile(member)
        item["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
        return archive

    def extract(self):
        return common.extract_payload(
            self.release, "x86_64-linux", "rime", self.root, self.root / "extracted"
        )

    def test_manifest_normalizes_native_build_and_rejects_untrusted_paths(self):
        path = self.root / "manifest.json"
        native = {**common.identity(self.release), "system": "x86_64-linux",
                  "artifacts": self.release["artifacts"]["x86_64-linux"]}
        path.write_text(json.dumps(native))
        loaded = common.load_release(path)
        self.assertEqual(set(loaded["artifacts"]), {"x86_64-linux"})
        native["artifacts"]["platform"]["file"] = "../../payload.tar.gz"
        path.write_text(json.dumps(native))
        with self.assertRaisesRegex(ValueError, "filename"):
            common.load_release(path)

    def test_modified_archive_is_rejected_before_extraction(self):
        archive = self.archive([])
        archive.write_bytes(archive.read_bytes() + b"modified")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.extract()
        self.assertFalse((self.root / "extracted").exists())

    def test_archive_paths_links_and_duplicate_files_cannot_escape_root(self):
        root = "oh-my-ime-rime-0.5.0-x86_64-linux"
        cases = [
            [(root + "/../../outside", b"data", tarfile.REGTYPE)],
            [("/outside", b"data", tarfile.REGTYPE)],
            [(root + "/link", b"../../outside", tarfile.SYMTYPE)],
            [(root + "/link", b"../../outside", tarfile.LNKTYPE)],
            [(root + "/fifo", b"", tarfile.FIFOTYPE)],
            [(root + "/file", b"a", tarfile.REGTYPE),
             (root + "/file", b"b", tarfile.REGTYPE)],
        ]
        for members in cases:
            with self.subTest(members=members):
                self.archive(members)
                with self.assertRaisesRegex(ValueError, "unsafe|duplicate"):
                    self.extract()
                self.assertFalse((self.root / "outside").exists())

    def test_correct_digest_does_not_override_architecture_check(self):
        header = bytearray(20)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, common.SYSTEMS["aarch64-linux"])
        self.archive([("oh-my-ime-rime-0.5.0-x86_64-linux/bin/omime-rime",
                       bytes(header), tarfile.REGTYPE)])
        with self.assertRaisesRegex(ValueError, "ELF for x86_64"):
            self.extract()

    def test_split_layout_has_no_shared_files_or_user_configuration(self):
        repository = Path(__file__).parents[2]
        outputs = {}
        for component in ("platform", "rime"):
            payload = self.root / component
            for relative in ("bin/omime-rime" if component == "rime" else "bin/omimed",
                             "share/licenses/oh-my-ime/LICENSE-MIT",
                             "share/licenses/oh-my-ime/LICENSE-APACHE", "README.txt"):
                path = payload / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("payload")
            engine = "rime" if component == "rime" else "echo"
            manifest = payload / f"share/oh-my-ime/engines/{engine}/engine.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(f'[engine]\nexec = ["omime-{engine}"]\n')
            if component == "platform":
                for source, target in (
                    ("data/omimed.service", "share/systemd/user/omimed.service"),
                    ("data/io.github.ign1x.OhMyIme.Settings.desktop",
                     "share/applications/io.github.ign1x.OhMyIme.Settings.desktop"),
                ):
                    path = payload / target
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes((repository / source).read_bytes())
            output = self.root / (component + "-stage")
            common.stage_component(payload, output, component)
            outputs[component] = {p.relative_to(output) for p in output.rglob("*") if p.is_file()}
            self.assertTrue(all(str(p).startswith("usr/") for p in outputs[component]))
            self.assertIn(f'["/usr/bin/omime-{engine}"]',
                          (output / f"usr/share/oh-my-ime/engines/{engine}/engine.toml").read_text())
        self.assertFalse(outputs["platform"] & outputs["rime"])
        unit = (self.root / "platform-stage/usr/lib/systemd/user/omimed.service").read_text()
        self.assertIn("ExecStart=/usr/bin/omimed", unit)
        self.assertIn("ExecReload=/usr/bin/omime reload", unit)
        self.assertNotIn("%h/.local", unit)

    def package_records(self):
        for system in common.SYSTEMS:
            for kind, count, suffix in (("deb", 2, ".deb"), ("rpm", 2, ".rpm"),
                                        ("appimage", 2, ".AppImage")):
                files = []
                for number in range(count):
                    filename = f"oh-my-ime-{number}-0.5.0-{system}{suffix}"
                    if kind == "appimage":
                        filename = f"oh-my-ime-0.5.0-{system}.AppImage" + (".build.json" if number else "")
                    path = self.root / filename
                    path.write_text(f"{system}/{kind}/{number}")
                    files.append(common.file_record(path))
                record = {**common.identity(self.release), "system": system,
                          "format": kind, "artifacts": files}
                (self.root / f"linux-{system}-{kind}.json").write_text(json.dumps(record))

    def test_assembler_requires_all_architectures_and_rejects_mixed_sources(self):
        self.package_records()
        packages = common.collect_packages(self.release, self.root)
        self.assertEqual(sum(len(files) for formats in packages.values()
                             for files in formats.values()), 12)
        path = self.root / "linux-aarch64-linux-rpm.json"
        data = json.loads(path.read_text())
        data["revision"] = "c" * 40
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "identity"):
            common.collect_packages(self.release, self.root)
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            common.collect_packages(self.release, self.root)

    def test_assembler_rejects_changed_packages_and_false_format_metadata(self):
        self.package_records()
        path = self.root / "linux-x86_64-linux-deb.json"
        data = json.loads(path.read_text())
        package = self.root / data["artifacts"][0]["file"]
        package.write_bytes(package.read_bytes() + b"modified")
        with self.assertRaisesRegex(ValueError, "checksum"):
            common.collect_packages(self.release, self.root)
        data["artifacts"][0] = common.file_record(package)
        data["format"] = "rpm"
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "format"):
            common.collect_packages(self.release, self.root)


if __name__ == "__main__":
    unittest.main()
