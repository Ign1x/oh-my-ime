#!/usr/bin/env python3
"""Build deb, RPM, AppImage or AUR recipes from checksum-verified release inputs."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile

from linux_packages.common import (
    SYSTEMS, collect_packages, extract_payload, file_record, identity, load_release, sha256,
)


def package(args):
    release = load_release(args.manifest)
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.format in {"aur", "assemble"}:
        from linux_packages import aur
        if args.format == "assemble":
            packages = collect_packages(release, args.out)
        files = aur.build(release, args.out)
        if args.format == "assemble":
            manifest = {**identity(release), "packages": packages,
                        "aur": file_record(files[0])}
            target = args.out / "linux-packages.json"
            target.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
            assets = [target, *files]
            assets.extend(args.out / item["file"] for formats in packages.values()
                          for artifacts in formats.values() for item in artifacts)
            (args.out / "SHA256SUMS-linux").write_text(
                "".join(f"{sha256(file)}  {file.name}\n" for file in sorted(assets))
            )
            print(f"Assembled {len(assets)} verified Linux packaging assets")
            return
    else:
        if args.system not in release["artifacts"]:
            raise ValueError("the requested architecture is missing from the manifest")
        with tempfile.TemporaryDirectory(prefix="omime-linux-package-") as temporary:
            payloads = {
                component: extract_payload(release, args.system, component, args.artifacts, temporary)
                for component in ("platform", "rime")
            }
            if args.format == "appimage":
                from linux_packages import appimage
                files = appimage.build(release, args.system, payloads, args.out)
            else:
                from linux_packages import native
                files = native.build(args.format, release, args.system, payloads, args.out)
    artifacts = []
    for file in files:
        file = Path(file)
        if file.parent.resolve() != args.out or not file.is_file():
            raise ValueError(f"builder returned an invalid output: {file}")
        artifacts.append(file_record(file))
    if not artifacts:
        raise ValueError("builder produced no package")
    record = {**identity(release), "format": args.format, "artifacts": artifacts}
    if args.format != "aur":
        record["system"] = args.system
    name = f"linux-{args.system}-{args.format}.json" if args.format != "aur" else "linux-aur.json"
    (args.out / name).write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    print(f"Built {args.format}: {', '.join(item['file'] for item in artifacts)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--format", required=True, choices=("deb", "rpm", "appimage", "aur", "assemble"))
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--artifacts", type=Path, default=Path("dist"))
    parser.add_argument("--system", choices=SYSTEMS)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.format not in {"aur", "assemble"} and args.system is None:
        parser.error("--system is required for native packages")
    try:
        package(args)
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"package-linux: {error}\n")


if __name__ == "__main__":
    main()
