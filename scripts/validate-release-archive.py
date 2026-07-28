#!/usr/bin/env python3
"""Validate one Codex Web Terminal release archive without trusting its paths."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import unicodedata
import zipfile


MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_EXTRACTED_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 20_000
MAX_ARCHIVE_PATH_BYTES = 4 * 1024
MAX_INDEX_BYTES = 1024 * 1024
MAX_BINARY_HEADER_BYTES = 1024 * 1024
COPY_CHUNK_BYTES = 1024 * 1024
ZIP_RATIO_MINIMUM_BYTES = 1024 * 1024
ZIP_RATIO_LIMIT = 1000
VITE_ASSET_PREFIX = "/assets/"
ASCII_WHITESPACE = frozenset(b" \t\n\r\x0b\x0c")


COMMON_REQUIRED_FILES = frozenset(
    {
        "AGENTS.md",
        "BUILDING.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "LICENSE",
        "OPERATIONS.md",
        "README.md",
        "release-package.json",
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
    if (
        not raw_name
        or len(raw_name.encode("utf-8")) > MAX_ARCHIVE_PATH_BYTES
        or "\0" in raw_name
        or "\\" in raw_name
        or raw_name.startswith("/")
        or any(unicodedata.category(character) == "Cc" for character in raw_name)
    ):
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
        or any(
            part.endswith((".", " "))
            or ":" in part
            or is_windows_reserved_name(part)
            for part in raw_parts
        )
    ):
        raise RuntimeError(f"archive contains an unsafe path: {raw_name!r}")
    return path.as_posix(), is_directory


def is_windows_reserved_name(value: str) -> bool:
    stem = value.split(".", 1)[0].upper()
    if stem in {"CON", "PRN", "AUX", "NUL"}:
        return True
    return len(stem) == 4 and stem[:3] in {"COM", "LPT"} and stem[3] in {
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "¹",
        "²",
        "³",
    }


def validate_member_names(
    members: list[tuple[str, bool]],
    expected_root: str,
) -> tuple[set[str], set[str]]:
    if (
        "/" in expected_root
        or "\\" in expected_root
        or expected_root in {"", ".", ".."}
        or canonical_member_name(expected_root) != (expected_root, False)
    ):
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


def visit_quoted_attributes(document: str, attribute: str) -> list[str]:
    values: list[str] = []
    name = attribute.encode("ascii")
    data = document.encode("utf-8")
    offset = 0
    while offset + len(name) <= len(data):
        if data[offset : offset + len(name)] != name or (
            offset > 0
            and data[offset - 1] != ord("<")
            and data[offset - 1] not in ASCII_WHITESPACE
        ):
            offset += 1
            continue

        cursor = offset + len(name)
        while cursor < len(data) and data[cursor] in ASCII_WHITESPACE:
            cursor += 1
        if cursor >= len(data) or data[cursor] != ord("="):
            offset += 1
            continue
        cursor += 1
        while cursor < len(data) and data[cursor] in ASCII_WHITESPACE:
            cursor += 1
        if cursor >= len(data) or data[cursor] not in {ord("'"), ord('"')}:
            raise RuntimeError(
                f"release package index.html contains an unquoted {attribute} attribute"
            )
        quote = data[cursor]
        value_start = cursor + 1
        try:
            value_end = data.index(quote, value_start)
        except ValueError as error:
            raise RuntimeError(
                f"release package index.html contains an unterminated {attribute} attribute"
            ) from error
        values.append(data[value_start:value_end].decode("utf-8"))
        offset = value_end + 1
    return values


def is_vite_hash(value: str) -> bool:
    return (
        8 <= len(value) <= 64
        and value.isascii()
        and all(character.isalnum() or character in "_-" for character in value)
    )


def validate_vite_asset_name(name: str) -> None:
    if (
        not name
        or len(name) > 255
        or not name.isascii()
        or any(
            not (character.isalnum() or character in "._-")
            for character in name
        )
    ):
        raise RuntimeError(
            "release package index.html contains an unsafe Vite asset name"
        )
    try:
        stem, extension = name.rsplit(".", 1)
    except ValueError as error:
        raise RuntimeError("release package Vite asset has no extension") from error
    if extension not in {"js", "css"} or not any(
        is_vite_hash(stem[index + 1 :])
        for index, character in enumerate(stem)
        if character == "-"
    ):
        raise RuntimeError(
            "release package index.html references a non-hashed Vite asset"
        )


def read_bounded_utf8(path: Path, maximum_bytes: int, label: str) -> str:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise RuntimeError(f"cannot inspect {label}: {error}") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_size > maximum_bytes
    ):
        raise RuntimeError(f"{label} is missing, unsafe, or too large")
    try:
        with path.open("rb") as source:
            payload = source.read(maximum_bytes + 1)
        if len(payload) > maximum_bytes:
            raise RuntimeError(f"{label} grew beyond the allowed size")
        return payload.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise RuntimeError(f"cannot read UTF-8 {label}: {error}") from error


def validate_frontend_assets(root: Path) -> None:
    web_root = root / "web"
    index = read_bounded_utf8(
        web_root / "index.html",
        MAX_INDEX_BYTES,
        "release package web/index.html",
    )
    assets: set[str] = set()
    has_javascript = False
    has_stylesheet = False
    for attribute in ("src", "href"):
        for value in visit_quoted_attributes(index, attribute):
            looks_like_code = value.endswith(".js") or value.endswith(".css")
            if not value.startswith(VITE_ASSET_PREFIX):
                if looks_like_code:
                    raise RuntimeError(
                        "release package index.html references unpackaged code"
                    )
                continue
            asset_name = value.removeprefix(VITE_ASSET_PREFIX)
            validate_vite_asset_name(asset_name)
            has_javascript = has_javascript or asset_name.endswith(".js")
            has_stylesheet = has_stylesheet or asset_name.endswith(".css")
            assets.add(asset_name)
    if not has_javascript or not has_stylesheet:
        raise RuntimeError(
            "release package index.html must reference hashed Vite "
            "JavaScript and CSS assets"
        )

    for asset_name in assets:
        asset = web_root / "assets" / asset_name
        try:
            mode = asset.lstat().st_mode
        except OSError as error:
            raise RuntimeError(
                f"release package index.html references a missing asset: {asset}"
            ) from error
        if not stat.S_ISREG(mode):
            raise RuntimeError(
                f"release package index.html references an unsafe asset: {asset}"
            )


def safe_target(extraction_root: Path, member_name: str) -> Path:
    target = extraction_root.joinpath(*PurePosixPath(member_name).parts)
    resolved_parent = target.parent.resolve()
    if extraction_root.resolve() != resolved_parent and extraction_root.resolve() not in resolved_parent.parents:
        raise RuntimeError(f"archive path escapes extraction root: {member_name!r}")
    return target


def bounded_copy(source: object, destination: object, expected_size: int) -> None:
    copied = 0
    while True:
        chunk = source.read(COPY_CHUNK_BYTES)
        if not chunk:
            break
        copied += len(chunk)
        if copied > expected_size or copied > MAX_EXTRACTED_BYTES:
            raise RuntimeError("archive entry expanded beyond its declared size")
        destination.write(chunk)
    if copied != expected_size:
        raise RuntimeError(
            f"archive entry size changed while extracting: {copied} != {expected_size}"
        )


def inspect_and_extract_zip(
    archive: Path,
    extraction_root: Path,
    expected_root: str,
    platform: str,
) -> None:
    with zipfile.ZipFile(archive) as package:
        entries: list[tuple[zipfile.ZipInfo, str, bool]] = []
        infos = package.infolist()
        if not infos or len(infos) > MAX_ARCHIVE_ENTRIES:
            raise RuntimeError("ZIP contains an invalid number of entries")
        total_size = 0
        for info in infos:
            name, trailing_directory = canonical_member_name(info.filename)
            is_directory = info.is_dir() or trailing_directory
            unix_mode = info.external_attr >> 16
            file_type = stat.S_IFMT(unix_mode)
            if file_type == stat.S_IFLNK:
                raise RuntimeError(f"ZIP contains a symbolic link: {name!r}")
            if file_type not in {0, stat.S_IFREG, stat.S_IFDIR}:
                raise RuntimeError(f"ZIP contains a special file: {name!r}")
            total_size += info.file_size
            if total_size > MAX_EXTRACTED_BYTES:
                raise RuntimeError("ZIP expands beyond the allowed limit")
            if (
                info.compress_size > 0
                and info.file_size > ZIP_RATIO_MINIMUM_BYTES
                and info.file_size // info.compress_size > ZIP_RATIO_LIMIT
            ):
                raise RuntimeError(
                    f"ZIP entry has an unsafe compression ratio: {name!r}"
                )
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
                bounded_copy(source, destination, info.file_size)


def inspect_and_extract_tar(
    archive: Path,
    extraction_root: Path,
    expected_root: str,
    platform: str,
) -> None:
    entries: list[tuple[str, bool, int, int]] = []
    with tarfile.open(archive, mode="r:gz") as package:
        total_size = 0
        for info in package:
            if len(entries) >= MAX_ARCHIVE_ENTRIES:
                raise RuntimeError("TAR contains too many entries")
            name, trailing_directory = canonical_member_name(info.name)
            is_directory = info.isdir() or trailing_directory
            if not (info.isfile() or is_directory):
                raise RuntimeError(f"TAR contains a link or special file: {name!r}")
            total_size += info.size
            if total_size > MAX_EXTRACTED_BYTES:
                raise RuntimeError("TAR expands beyond the allowed limit")
            entries.append((name, is_directory, info.size, info.mode))
    if not entries:
        raise RuntimeError("TAR is empty")

    files, _ = validate_member_names(
        [(name, is_directory) for name, is_directory, _, _ in entries],
        expected_root,
    )
    validate_required_files(files, expected_root, platform)

    with tarfile.open(archive, mode="r:gz") as package:
        extracted_count = 0
        for info, expected in zip(package, entries, strict=True):
            name, is_directory, expected_size, expected_mode = expected
            observed_name, trailing_directory = canonical_member_name(info.name)
            if (
                observed_name != name
                or (info.isdir() or trailing_directory) != is_directory
                or (not is_directory and not info.isfile())
                or info.size != expected_size
                or info.mode != expected_mode
            ):
                raise RuntimeError("TAR metadata changed between validation and extraction")
            target = safe_target(extraction_root, name)
            if is_directory:
                target.mkdir(parents=True, exist_ok=True)
            else:
                source = package.extractfile(info)
                if source is None:
                    raise RuntimeError(f"TAR file has no readable payload: {name!r}")
                target.parent.mkdir(parents=True, exist_ok=True)
                with source, target.open("xb") as destination:
                    bounded_copy(source, destination, expected_size)
                target.chmod(expected_mode & 0o777)
            extracted_count += 1
        if extracted_count != len(entries):
            raise RuntimeError("TAR entry count changed between validation and extraction")


def validate_binary_architecture(binary: Path, platform: str) -> None:
    try:
        with binary.open("rb") as source:
            data = source.read(MAX_BINARY_HEADER_BYTES)
    except OSError as error:
        raise RuntimeError(f"cannot inspect release executable {binary}: {error}") from error
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


def validate_release_package_manifest(
    root: Path,
    platform: str,
    expected_version: str,
) -> None:
    path = root / "release-package.json"
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or set(manifest) != {
        "schemaVersion",
        "product",
        "version",
        "target",
    }:
        raise RuntimeError("release package marker has an invalid schema")
    expected_target = (
        "x86_64-pc-windows-msvc"
        if platform == "windows"
        else "x86_64-unknown-linux-gnu"
    )
    expected = {
        "schemaVersion": 1,
        "product": "codex-web-terminal",
        "version": expected_version,
        "target": expected_target,
    }
    if manifest != expected:
        raise RuntimeError(
            f"release package marker is {manifest!r}, expected {expected!r}"
        )


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
    supplied_archive = args.archive
    try:
        archive_metadata = supplied_archive.lstat()
    except OSError as error:
        raise RuntimeError(
            f"release archive is missing or unsafe: {supplied_archive}"
        ) from error
    if not stat.S_ISREG(archive_metadata.st_mode):
        raise RuntimeError(f"release archive is missing or unsafe: {supplied_archive}")
    if archive_metadata.st_size > MAX_ARCHIVE_BYTES:
        raise RuntimeError(
            f"release archive exceeds the {MAX_ARCHIVE_BYTES}-byte limit: {supplied_archive}"
        )
    archive = supplied_archive.resolve()

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
        validate_frontend_assets(root)
        validate_binary_architecture(binary, args.platform)
        validate_license_manifest(root, args.platform)
        validate_release_package_manifest(
            root,
            args.platform,
            args.expected_version,
        )
        if args.execute_smoke:
            execute_smoke(binary, args.platform, args.expected_version)


def main() -> int:
    args = parse_args()
    validate_archive(args)
    print(f"Validated release archive: {args.archive.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
