#!/usr/bin/env python3
"""Validate one Codex Web Terminal release archive without trusting its paths."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import zipfile


COMMON_REQUIRED_FILES = frozenset(
    {
        "AGENTS.md",
        "BUILDING.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "LICENSE",
        "OPERATIONS.md",
        "README.md",
        "SECURITY.md",
        "THIRD_PARTY_NOTICES.md",
        "TODO.md",
        "THIRD_PARTY_LICENSES/THIRD_PARTY_LICENSES.txt",
        "THIRD_PARTY_LICENSES/manifest.json",
        "docs/screenshots/README.md",
        "web/index.html",
    }
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Reject unsafe or incomplete release archives, extract them into a "
            "temporary directory, and optionally smoke-test the native binary."
        )
    )
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--expected-root", required=True)
    parser.add_argument("--platform", choices=("windows", "linux"), required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--execute-smoke", action="store_true")
    return parser.parse_args()


def canonical_member_name(raw_name: str) -> tuple[str, bool]:
    if not raw_name or "\0" in raw_name or "\\" in raw_name:
        raise RuntimeError(f"archive contains a non-canonical path: {raw_name!r}")
    is_directory = raw_name.endswith("/")
    name = raw_name.rstrip("/")
    raw_parts = name.split("/")
    path = PurePosixPath(name)
    if (
        not name
        or path.is_absolute()
        or path.anchor
        or any(part in {"", ".", ".."} for part in raw_parts)
        or (path.parts and ":" in path.parts[0])
    ):
        raise RuntimeError(f"archive contains an unsafe path: {raw_name!r}")
    return path.as_posix(), is_directory


def validate_member_names(
    members: list[tuple[str, bool]],
    expected_root: str,
) -> tuple[set[str], set[str]]:
    if "/" in expected_root or "\\" in expected_root or expected_root in {"", ".", ".."}:
        raise RuntimeError("the expected archive root is not a single safe directory name")

    files: set[str] = set()
    directories: set[str] = set()
    casefolded: dict[str, str] = {}
    roots: set[str] = set()
    for name, is_directory in members:
        path = PurePosixPath(name)
        roots.add(path.parts[0])
        folded = name.casefold()
        previous = casefolded.get(folded)
        if previous is not None:
            raise RuntimeError(
                f"archive contains duplicate or case-colliding paths: {previous!r}, {name!r}"
            )
        casefolded[folded] = name
        (directories if is_directory else files).add(name)

    if roots != {expected_root}:
        raise RuntimeError(
            f"archive must contain exactly the root {expected_root!r}; found {sorted(roots)!r}"
        )
    return files, directories


def required_paths(expected_root: str, platform: str) -> set[str]:
    binary = "codex-web.exe" if platform == "windows" else "codex-web"
    return {f"{expected_root}/{path}" for path in COMMON_REQUIRED_FILES | {binary}}


def validate_required_files(
    files: set[str],
    expected_root: str,
    platform: str,
) -> None:
    missing = sorted(required_paths(expected_root, platform) - files)
    if missing:
        raise RuntimeError(f"archive is missing required files: {missing!r}")
    asset_prefix = f"{expected_root}/web/assets/"
    if not any(path.startswith(asset_prefix) for path in files):
        raise RuntimeError("archive contains no production browser assets")


def safe_target(extraction_root: Path, member_name: str) -> Path:
    target = extraction_root.joinpath(*PurePosixPath(member_name).parts)
    resolved_parent = target.parent.resolve()
    if extraction_root.resolve() != resolved_parent and extraction_root.resolve() not in resolved_parent.parents:
        raise RuntimeError(f"archive path escapes extraction root: {member_name!r}")
    return target


def inspect_and_extract_zip(
    archive: Path,
    extraction_root: Path,
    expected_root: str,
    platform: str,
) -> None:
    with zipfile.ZipFile(archive) as package:
        entries: list[tuple[zipfile.ZipInfo, str, bool]] = []
        for info in package.infolist():
            name, trailing_directory = canonical_member_name(info.filename)
            is_directory = info.is_dir() or trailing_directory
            unix_mode = info.external_attr >> 16
            file_type = stat.S_IFMT(unix_mode)
            if file_type == stat.S_IFLNK:
                raise RuntimeError(f"ZIP contains a symbolic link: {name!r}")
            if file_type not in {0, stat.S_IFREG, stat.S_IFDIR}:
                raise RuntimeError(f"ZIP contains a special file: {name!r}")
            entries.append((info, name, is_directory))

        files, _ = validate_member_names(
            [(name, is_directory) for _, name, is_directory in entries],
            expected_root,
        )
        validate_required_files(files, expected_root, platform)

        for info, name, is_directory in entries:
            target = safe_target(extraction_root, name)
            if is_directory:
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            with package.open(info) as source, target.open("xb") as destination:
                shutil.copyfileobj(source, destination)


def inspect_and_extract_tar(
    archive: Path,
    extraction_root: Path,
    expected_root: str,
    platform: str,
) -> None:
    with tarfile.open(archive, mode="r:gz") as package:
        entries: list[tuple[tarfile.TarInfo, str, bool]] = []
        for info in package.getmembers():
            name, trailing_directory = canonical_member_name(info.name)
            is_directory = info.isdir() or trailing_directory
            if not (info.isfile() or is_directory):
                raise RuntimeError(f"TAR contains a link or special file: {name!r}")
            entries.append((info, name, is_directory))

        files, _ = validate_member_names(
            [(name, is_directory) for _, name, is_directory in entries],
            expected_root,
        )
        validate_required_files(files, expected_root, platform)

        for info, name, is_directory in entries:
            target = safe_target(extraction_root, name)
            if is_directory:
                target.mkdir(parents=True, exist_ok=True)
                continue
            source = package.extractfile(info)
            if source is None:
                raise RuntimeError(f"TAR file has no readable payload: {name!r}")
            target.parent.mkdir(parents=True, exist_ok=True)
            with source, target.open("xb") as destination:
                shutil.copyfileobj(source, destination)
            target.chmod(info.mode & 0o777)


def validate_binary_architecture(binary: Path, platform: str) -> None:
    data = binary.read_bytes()
    if platform == "windows":
        if len(data) < 0x40 or data[:2] != b"MZ":
            raise RuntimeError("Windows executable has no valid DOS/PE header")
        pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
        if (
            pe_offset + 6 > len(data)
            or data[pe_offset : pe_offset + 4] != b"PE\0\0"
            or struct.unpack_from("<H", data, pe_offset + 4)[0] != 0x8664
        ):
            raise RuntimeError("Windows executable is not PE x86-64")
        return

    if (
        len(data) < 20
        or data[:4] != b"\x7fELF"
        or data[4] != 2
        or data[5] != 1
        or struct.unpack_from("<H", data, 18)[0] != 62
    ):
        raise RuntimeError("Linux executable is not little-endian ELF x86-64")


def validate_license_manifest(root: Path, platform: str) -> None:
    path = root / "THIRD_PARTY_LICENSES" / "manifest.json"
    manifest = json.loads(path.read_text(encoding="utf-8"))
    expected_target = (
        "x86_64-pc-windows-msvc"
        if platform == "windows"
        else "x86_64-unknown-linux-gnu"
    )
    if manifest.get("target") != expected_target:
        raise RuntimeError(
            f"license manifest target is {manifest.get('target')!r}, "
            f"expected {expected_target!r}"
        )
    packages = manifest.get("packages")
    if not isinstance(packages, list) or not packages:
        raise RuntimeError("license manifest contains no package inventory")


def execute_smoke(binary: Path, platform: str, expected_version: str) -> None:
    native = (platform == "windows" and os.name == "nt") or (
        platform == "linux" and sys.platform.startswith("linux")
    )
    if not native:
        raise RuntimeError(
            f"cannot execute a {platform} binary on {sys.platform}/{os.name}"
        )
    result = subprocess.run(
        [str(binary), "--version"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        timeout=15,
    )
    actual = result.stdout.strip()
    expected = f"codex-web {expected_version}"
    if actual != expected:
        raise RuntimeError(f"binary version is {actual!r}, expected {expected!r}")


def validate_archive(args: argparse.Namespace) -> None:
    archive = args.archive.resolve()
    if not archive.is_file() or archive.is_symlink():
        raise RuntimeError(f"release archive is missing or unsafe: {archive}")

    with tempfile.TemporaryDirectory(prefix="codex-web-release-check-") as temporary:
        extraction_root = Path(temporary).resolve()
        if archive.name.endswith(".zip"):
            inspect_and_extract_zip(
                archive,
                extraction_root,
                args.expected_root,
                args.platform,
            )
        elif archive.name.endswith(".tar.gz"):
            inspect_and_extract_tar(
                archive,
                extraction_root,
                args.expected_root,
                args.platform,
            )
        else:
            raise RuntimeError("release archive must be .zip or .tar.gz")

        root = extraction_root / args.expected_root
        binary = root / (
            "codex-web.exe" if args.platform == "windows" else "codex-web"
        )
        validate_binary_architecture(binary, args.platform)
        validate_license_manifest(root, args.platform)
        if args.execute_smoke:
            execute_smoke(binary, args.platform, args.expected_version)


def main() -> int:
    args = parse_args()
    validate_archive(args)
    print(f"Validated release archive: {args.archive.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
