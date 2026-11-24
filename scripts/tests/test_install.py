"""Install prebuilt fixtures into a private prefix and validate desktop discovery."""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET


class NativeInstallTests(unittest.TestCase):
    def test_native_registration_quotes_absolute_paths_without_autostart(self):
        repository = Path(__file__).parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            for program in ("omimed", "omime", "omime-ibus", "omime-panel", "omime-settings", "omime-echo", "omime-sparkle"):
                path = binaries / program
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            prefix = root / "local with & <xml> 'quotes' %i"
            result = subprocess.run(["sh", str(repository / "scripts/install.sh")],
                                    cwd=repository, env=os.environ | {
                                        "PREFIX": str(prefix), "OMIME_INSTALL_BIN_DIR": str(binaries),
                                    }, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            for name, arguments in (("InputMethod", []), ("IBus", ["--ibus"])):
                desktop = prefix / f"share/applications/io.github.ign1x.OhMyIme.{name}.desktop"
                command = next(line[5:] for line in desktop.read_text().splitlines() if line.startswith("Exec="))
                self.assertEqual(shlex.split(command.replace("%%", "%")), [str(prefix / "bin/omimed"), *arguments])
            component = ET.parse(prefix / "share/ibus/component/oh-my-ime.xml")
            self.assertEqual(shlex.split(component.findtext("exec")), [str(prefix / "bin/omimed"), "--ibus"])
            self.assertFalse((prefix / "etc/xdg/autostart").exists())
            self.assertFalse((prefix / "share/autostart").exists())


if __name__ == "__main__":
    unittest.main()
