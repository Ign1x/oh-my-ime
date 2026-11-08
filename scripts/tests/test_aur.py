"""AUR source/staging checks, plus real version comparisons when vercmp exists."""

import copy
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("aur", ROOT / "scripts/linux_packages/aur.py")
aur = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(aur)
VERIFY_SPEC = importlib.util.spec_from_file_location("aur_verify", ROOT / "packaging/aur/verify.py")
aur_verify = importlib.util.module_from_spec(VERIFY_SPEC)
VERIFY_SPEC.loader.exec_module(aur_verify)
VERCMP = os.environ.get("AUR_VERCMP") or shutil.which("vercmp")


class AurTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.release = {
            "version": "1.2.3", "tag": "v1.2.3", "revision": "a" * 40,
            "repository": "Ign1x/oh-my-ime", "artifacts": {},
        }
        for index, arch in enumerate(aur.ARCHITECTURES):
            self.release["artifacts"][f"{arch}-linux"] = {
                component: {
                    "file": f"{prefix}-1.2.3-{arch}-linux.tar.gz",
                    "sha256": str(index * 2 + offset + 1) * 64,
                }
                for offset, (component, prefix) in enumerate((
                    ("platform", "oh-my-ime"), ("rime", "oh-my-ime-rime"),
                ))
            }

    def generate(self, release=None):
        [archive] = aur.build(release or self.release, self.root)
        with tarfile.open(archive) as source:
            files = {
                name: source.extractfile(f"oh-my-ime-bin/{name}").read().decode()
                for name in ("PKGBUILD", ".SRCINFO")
            }
        return archive, files

    def release_for(self, version):
        release = copy.deepcopy(self.release)
        release.update(version=version, tag=f"v{version}")
        for components in release["artifacts"].values():
            for artifact in components.values():
                artifact["file"] = artifact["file"].replace("1.2.3", version)
        return release

    def test_release_sources_are_complete_pinned_and_reproducible(self):
        archive, files = self.generate()
        first = archive.read_bytes()
        self.generate()
        self.assertEqual(archive.read_bytes(), first)
        for arch in aur.ARCHITECTURES:
            self.assertIn(f"arch = {arch}", files[".SRCINFO"])
            for item in self.release["artifacts"][f"{arch}-linux"].values():
                url = f"https://github.com/Ign1x/oh-my-ime/releases/download/v1.2.3/{item['file']}"
                for text in files.values():
                    self.assertIn(url, text)
                    self.assertIn(item["sha256"], text)
                    self.assertNotIn("SKIP", text)
        self.assertIn("depends = oh-my-ime-bin=1.2.3-1", files[".SRCINFO"])
        for package in ("oh-my-ime", "oh-my-ime-rime"):
            self.assertIn(f"provides = {package}=1.2.3", files[".SRCINFO"])
            self.assertIn(f"conflicts = {package}", files[".SRCINFO"])

    def test_partial_or_inconsistent_release_cannot_generate_a_recipe(self):
        variants = []
        release = copy.deepcopy(self.release)
        del release["artifacts"]["aarch64-linux"]
        variants.append(release)
        for field, value in (
            ("sha256", "SKIP"),
            ("file", "../archive.tar.gz"),
            ("url", "https://example.org/unrelated.tar.gz"),
        ):
            release = copy.deepcopy(self.release)
            release["artifacts"]["x86_64-linux"]["platform"][field] = value
            variants.append(release)
        release = copy.deepcopy(self.release)
        release["repository"] = "owner/repo;false"
        variants.append(release)
        for release in variants:
            with self.subTest(release=release), self.assertRaises(ValueError):
                aur.build(release, self.root)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_prerelease_version_keeps_original_source_names(self):
        for version, pkgver in (
            ("1.2.3-rc.1", "1.2.3prerc.1"),
            ("1.2.3-0", "1.2.3pre0"),
            ("1.2.3-01", "1.2.3pre01"),
            ("1.2.3-alpha-beta.2", "1.2.3prealpha_beta.2"),
            ("1.2.3-1-rc.2", "1.2.3pre1_rc.2"),
        ):
            with self.subTest(version=version):
                archive, files = self.generate(self.release_for(version))
                self.assertEqual(archive.name, f"oh-my-ime-aur-{version}.tar.gz")
                self.assertIn(f"pkgver = {pkgver}", files[".SRCINFO"])
                self.assertIn(f"depends = oh-my-ime-bin={pkgver}-1", files[".SRCINFO"])
                self.assertIn(f"/v{version}/oh-my-ime-{version}-", files["PKGBUILD"])
                self.assertIn(f"_release_version='{version}'", files["PKGBUILD"])
                self.assertEqual(aur_verify.package_version(version), pkgver)
        self.assertEqual(aur_verify.package_version("1.2.3"), "1.2.3")

    @unittest.skipUnless(VERCMP, "requires Arch vercmp (or AUR_VERCMP executable)")
    def test_prereleases_upgrade_to_stable_and_sequence_with_real_vercmp(self):
        def mapped(version):
            return aur._metadata(self.release_for(version))[0]["pkgver"] + "-1"

        def check_older(older, newer):
            left, right = mapped(older), mapped(newer)
            with self.subTest(older=older, newer=newer):
                actual = subprocess.check_output([VERCMP, left, right], text=True).strip()
                self.assertEqual(actual, "-1", f"vercmp {left} {right}: {actual}")

        labels = (
            "0", "01", "1", "9", "10", "alpha", "alpha.1", "alpha.2", "alpha.10",
            "beta", "beta.2", "rc", "rc.1", "rc.2", "rc.10", "rc-1", "rc-2", "rc-10",
            "alpha-beta.1", "alpha-beta.2", "1-foo", "1-foo.1", "foo-bar", "-rc", "--", ".",
        )
        for label in labels:
            check_older(f"1.2.3-{label}", "1.2.3")
        for sequence in (
            ("0", "1", "9", "10", "alpha", "alpha.1", "alpha.2", "alpha.10", "beta", "beta.2", "rc", "rc.1", "rc.2", "rc.10"),
            ("rc-1", "rc-2", "rc-10"),
            ("alpha-beta.1", "alpha-beta.2"),
            ("1-foo", "1-foo.1"),
        ):
            for older, newer in zip(sequence, sequence[1:]):
                check_older(f"1.2.3-{older}", f"1.2.3-{newer}")
        check_older("1.2.2", "1.2.3-alpha")
        check_older("1.2.3", "1.2.4-alpha")

    def test_split_packages_install_resolved_paths_without_file_collisions(self):
        _, files = self.generate()
        recipe = self.root / "PKGBUILD"
        recipe.write_text(files["PKGBUILD"])
        installed = {}
        for component, prefix in (("platform", "oh-my-ime"), ("rime", "oh-my-ime-rime")):
            payload = self.root / "src" / f"{prefix}-1.2.3-x86_64-linux"
            engines = ("echo", "sparkle", "ibus", "fcitx5") if component == "platform" else ("rime",)
            for engine in engines:
                source = ROOT / ("data/engines" if engine in ("ibus", "fcitx5") else "engines")
                target = payload / "share/oh-my-ime/engines" / engine / "engine.toml"
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source / engine / "engine.toml", target)
            (payload / "bin").mkdir()
            programs = ("omimed", "omime", "omime-ibus", "omime-settings", "omime-echo", "omime-sparkle") \
                if component == "platform" else ("omime-rime",)
            for program in programs:
                (payload / "bin" / program).write_text("fixture\n")
            licenses = payload / "share/licenses/oh-my-ime"
            licenses.mkdir(parents=True)
            for name in ("LICENSE-MIT", "LICENSE-APACHE"):
                shutil.copyfile(ROOT / name, licenses / name)
            (payload / "README.txt").write_text("Release readme\n")
            if component == "platform":
                for name, relative in (
                    ("omimed.service", "systemd/user/omimed.service"),
                    ("io.github.ign1x.OhMyIme.Settings.desktop",
                     "applications/io.github.ign1x.OhMyIme.Settings.desktop"),
                    ("config.toml", "oh-my-ime/config.toml"),
                ):
                    target = payload / "share" / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ROOT / "data" / name, target)
            destination = self.root / f"{component}-installed"
            subprocess.run([
                "bash", "-eu", "-c",
                'source "$1"; srcdir=$2; pkgdir=$3; CARCH=x86_64; "package_$4"',
                "aur-test", str(recipe), str(self.root / "src"), str(destination), f"{prefix}-bin",
            ], check=True)
            installed[component] = destination

        platform, rime = installed["platform"], installed["rime"]
        platform_files = {p.relative_to(platform) for p in platform.rglob("*") if p.is_file()}
        rime_files = {p.relative_to(rime) for p in rime.rglob("*") if p.is_file()}
        self.assertFalse(platform_files & rime_files)
        self.assertTrue(all(p.parts[0] == "usr" for p in platform_files | rime_files))
        unit = (platform / "usr/lib/systemd/user/omimed.service").read_text()
        self.assertIn("ExecStart=/usr/bin/omimed", unit)
        self.assertIn("ExecReload=/usr/bin/omime reload", unit)
        desktop = (platform / "usr/share/applications/io.github.ign1x.OhMyIme.Settings.desktop").read_text()
        self.assertIn("Exec=/usr/bin/omime-settings", desktop)
        for engine, destination in (("echo", platform), ("sparkle", platform), ("ibus", platform), ("rime", rime)):
            manifest = (destination / "usr/share/oh-my-ime/engines" / engine / "engine.toml").read_text()
            self.assertIn(f'exec = ["/usr/bin/omime-{engine}"', manifest)
        self.assertEqual(
            (rime / "usr/share/oh-my-ime/engines/rime/engine.toml").read_text().count('"/usr/bin/omime-rime"'), 2,
        )
        self.assertFalse((platform / "usr/share/systemd").exists())
        self.assertTrue((platform / "usr/share/licenses/oh-my-ime-bin/LICENSE-MIT").is_file())
        self.assertTrue((rime / "usr/share/licenses/oh-my-ime-rime-bin/LICENSE-MIT").is_file())


if __name__ == "__main__":
    unittest.main()
