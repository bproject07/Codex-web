#!/usr/bin/env python3
"""Write the marker that makes an official package eligible for self-update."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re


SCHEMA_VERSION = 1
PRODUCT = "codex-web-terminal"
TARGETS = {
    "x86_64-pc-windows-msvc": "codex-web.exe",
    "x86_64-unknown-linux-gnu": "codex-web",
}
VERSION_PATTERN = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
MANIFEST_NAME = "release-package.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Add a deterministic release identity marker to an already built "
            "and licensed Codex Web Terminal package."
        )
    )
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", choices=sorted(TARGETS), required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def build_manifest(version: str, target: str) -> dict[str, object]:
    if VERSION_PATTERN.fullmatch(version) is None:
        raise RuntimeError("release package version must be stable MAJOR.MINOR.PATCH")
    if target not in TARGETS:
        raise RuntimeError(f"unsupported release target: {target!r}")
    return {
        "schemaVersion": SCHEMA_VERSION,
        "product": PRODUCT,
        "version": version,
        "target": target,
    }


def write_manifest(output_dir: Path, version: str, target: str) -> Path:
    if not output_dir.is_dir() or output_dir.is_symlink():
        raise RuntimeError(f"release package directory is missing or unsafe: {output_dir}")
    required = (
        output_dir / TARGETS[target],
        output_dir / "web" / "index.html",
        output_dir / "THIRD_PARTY_LICENSES" / "manifest.json",
    )
    for path in required:
        if not path.is_file() or path.is_symlink():
            raise RuntimeError(f"release package is incomplete or unsafe: {path}")

    destination = output_dir / MANIFEST_NAME
    if destination.exists() or destination.is_symlink():
        raise RuntimeError(f"refusing to replace release package marker: {destination}")
    payload = json.dumps(
        build_manifest(version, target),
        indent=2,
        ensure_ascii=False,
    ) + "\n"
    destination.write_text(payload, encoding="utf-8", newline="\n")
    return destination


def main() -> int:
    args = parse_args()
    destination = write_manifest(
        args.output_dir.resolve(),
        args.version,
        args.target,
    )
    print(f"Generated release package marker: {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
