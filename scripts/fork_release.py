#!/usr/bin/env python3
"""Stamp fork package metadata and validate/publish a paired GitHub Release.

Tags use fork-vMAJOR.MINOR.PATCH.BUILD. BUILD is strictly increasing within
the same upstream version and must match both manually dispatched builds.
"""

import argparse
import hashlib
import json
import re
import tarfile
import zipfile
from pathlib import Path

TAG_RE = re.compile(r"^fork-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\.([1-9]\d*)$")
TARGETS = {
    "windows": ("x86_64-pc-windows-msvc", "codex-windows-x64.zip"),
    "macos": ("x86_64-apple-darwin", "codex-macos-intel.tar.gz"),
}


def stamp(package_dir: Path, build: int) -> None:
    if build < 1:
        raise ValueError("Fork build number must be a positive integer")
    manifest_path = package_dir / "codex-package.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("target") not in [target for target, _ in TARGETS.values()]:
        raise ValueError("Unsupported fork package target")
    manifest["forkBuildNumber"] = build
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def read_archive_manifest(archive: Path, kind: str) -> dict:
    if kind == "windows":
        with zipfile.ZipFile(archive) as z:
            names = set(z.namelist())
            if "bin/codex.exe" not in names:
                raise ValueError("Windows archive lacks codex.exe")
            return json.loads(z.read("codex-package.json"))
    with tarfile.open(archive, "r:gz") as t:
        names = set(t.getnames())
        if "bin/codex" not in names:
            raise ValueError("macOS archive lacks bin/codex")
        stream = t.extractfile("codex-package.json")
        if stream is None:
            raise ValueError("macOS archive lacks codex-package.json")
        return json.load(stream)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_manifest(tag: str, windows: Path, macos: Path) -> dict:
    match = TAG_RE.fullmatch(tag)
    if not match:
        raise ValueError("Tag must look like fork-v0.167.0.1")
    version = ".".join(match.groups()[:3])
    build = int(match.group(4))
    assets = {}
    for kind, archive in [("windows", windows), ("macos", macos)]:
        expected_target, expected_name = TARGETS[kind]
        if archive.name != expected_name:
            raise ValueError(f"Expected {expected_name}, got {archive.name}")
        package = read_archive_manifest(archive, kind)
        if (
            package.get("target") != expected_target
            or package.get("version") != version
            or package.get("forkBuildNumber") != build
        ):
            raise ValueError(f"{kind} package metadata does not match tag {tag}")
        assets[expected_target] = {"name": expected_name, "sha256": sha256_file(archive)}
    return {"schema": 1, "version": version, "build": build, "assets": assets}


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p_stamp = sub.add_parser("stamp")
    p_stamp.add_argument("--package-dir", required=True, type=Path)
    p_stamp.add_argument("--build", required=True, type=int)
    p_publish = sub.add_parser("manifest")
    p_publish.add_argument("--tag", required=True)
    p_publish.add_argument("--windows", required=True, type=Path)
    p_publish.add_argument("--macos", required=True, type=Path)
    p_publish.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.command == "stamp":
        stamp(args.package_dir, args.build)
    else:
        result = release_manifest(args.tag, args.windows, args.macos)
        args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(f"Validated fork release {args.tag}")


if __name__ == "__main__":
    main()
