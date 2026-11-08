#!/usr/bin/env python3
"""Download existing release archives for a distribution-packaging dry run."""

import argparse
from pathlib import Path
import re
import shutil
import tempfile
import urllib.request

from linux_packages.common import SYSTEMS, load_release, sha256


def download(url, target):
    request = urllib.request.Request(url, headers={"User-Agent": "oh-my-ime-packaging"})
    with urllib.request.urlopen(request, timeout=180) as response, target.open("wb") as output:
        shutil.copyfileobj(response, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repository", default="Ign1x/oh-my-ime")
    parser.add_argument("--system", choices=SYSTEMS)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", args.tag):
        parser.error("--tag must be a version tag")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
        parser.error("--repository must be OWNER/REPO")
    args.out.mkdir(parents=True, exist_ok=True)
    base = f"https://github.com/{args.repository}/releases/download/{args.tag}"
    with tempfile.TemporaryDirectory(prefix=".fetch-", dir=args.out) as temporary:
        work = Path(temporary)
        manifest = work / "release.json"
        download(f"{base}/release.json", manifest)
        release = load_release(manifest)
        if release["tag"] != args.tag or release["repository"] != args.repository:
            raise ValueError("downloaded manifest does not match the requested release")
        for system in ([args.system] if args.system else SYSTEMS):
            for artifact in release["artifacts"][system].values():
                destination = args.out / artifact["file"]
                if destination.is_file() and sha256(destination) == artifact["sha256"]:
                    continue
                target = work / artifact["file"]
                download(f"{base}/{artifact['file']}", target)
                if sha256(target) != artifact["sha256"]:
                    raise ValueError(f"{artifact['file']}: downloaded checksum mismatch")
                target.replace(destination)
                print(f"Verified {artifact['file']}", flush=True)
        manifest.replace(args.out / "release.json")


if __name__ == "__main__":
    main()
