#!/usr/bin/env python3
"""Build the pinned librime in a private prefix on native Ubuntu 24.04.

Ubuntu 24.04 ships librime 1.10, while omime-rime needs the 1.11 change_page API.
This command never installs into the system and never invokes sudo or apt.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--source", type=Path, help="use this archive instead of downloading; its SHA-256 is still checked")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    host = platform.freedesktop_os_release()
    if (host.get("ID"), host.get("VERSION_ID")) != ("ubuntu", "24.04"):
        parser.error("run on native Ubuntu 24.04 to preserve the glibc baseline")
    pin = json.loads(Path(__file__).with_name("rime-source.json").read_text())
    prefix = args.prefix.absolute()
    if prefix.exists() or prefix.is_symlink():
        parser.error(f"refusing to replace existing prefix {prefix}; choose an empty path")
    prefix.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".omime-rime-", dir=prefix.parent) as temporary:
        work = Path(temporary)
        archive = work / "source.tar.gz"
        if args.source:
            shutil.copyfile(args.source, archive)
        else:
            with urllib.request.urlopen(pin["url"], timeout=120) as response, archive.open("wb") as output:
                shutil.copyfileobj(response, output)
        if sha256(archive) != pin["sha256"]:
            raise ValueError("librime source SHA-256 mismatch")
        with tarfile.open(archive) as source:
            source.extractall(work / "source", filter="data")
        source = work / "source" / f"librime-{pin['version']}"
        build = work / "build"
        flags = f"-ffile-prefix-map={work}=."
        subprocess.run([
            "cmake", "-S", str(source), "-B", str(build), "-G", "Ninja",
            "-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_SKIP_RPATH=TRUE", f"-DCMAKE_CXX_FLAGS={flags}",
            "-DBUILD_TEST=OFF", "-DBUILD_DATA=OFF", "-DBUILD_STATIC=OFF", "-DENABLE_EXTERNAL_PLUGINS=OFF",
        ], check=True)
        subprocess.run(["cmake", "--build", str(build), "--target", "rime", "--parallel", str(args.jobs)], check=True)
        stage = work / "prefix"
        (stage / "lib").mkdir(parents=True)
        library = stage / "lib/librime.so.1"
        shutil.copy2(build / "lib" / f"librime.so.{pin['version']}", library)
        subprocess.run(["strip", "--strip-unneeded", str(library)], check=True)
        probe = work / "check-api.cpp"
        probe.write_text(
            '#include <rime_api.h>\n#include <cstdio>\n'
            'int main() { auto api = rime_get_api(); '
            'if (!RIME_API_AVAILABLE(api, change_page)) return 1; '
            'std::puts(api->get_version()); }\n'
        )
        subprocess.run([
            "c++", "-I", str(source / "src"), str(probe), str(library),
            f"-Wl,-rpath,{stage / 'lib'}", "-o", str(work / "check-api"),
        ], check=True)
        actual = subprocess.check_output([str(work / "check-api")], text=True).strip()
        if actual != pin["version"]:
            raise ValueError(f"librime build reports {actual}, expected {pin['version']}")
        licenses = stage / "share/licenses/librime"
        licenses.mkdir(parents=True)
        shutil.copy2(source / "LICENSE", licenses / "LICENSE")
        record = {
            "source": pin, "library_sha256": sha256(library), "architecture": platform.machine(),
            "compiler": subprocess.check_output(["c++", "--version"], text=True).splitlines()[0],
            "features": {"external_plugins": False, "shared_library": True},
        }
        (stage / "rime-source.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        # A failed compile or API check leaves no partially prepared prefix.
        stage.rename(prefix)
    print(f"Prepared librime {actual}: OMIME_APPIMAGE_RIME_PREFIX={prefix}")


if __name__ == "__main__":
    main()
