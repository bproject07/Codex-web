#!/usr/bin/env python3
"""Fail-closed validation for GitHub immutable-release metadata.

The release workflow obtains JSON through ``gh api``. Keeping authentication
and network retries in the workflow lets this helper remain deterministic and
unit-testable while still validating every security-relevant field.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
import sys
from typing import Any


MAX_METADATA_BYTES = 8 * 1024 * 1024
SHA256_RE = re.compile(r"sha256:([0-9a-f]{64})")
CHECKSUM_LINE_RE = re.compile(r"([0-9a-f]{64})  ([^/\r\n]+)")


class VerificationError(RuntimeError):
    """Metadata or local assets do not satisfy the release contract."""


def load_metadata(path: Path) -> dict[str, Any]:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise VerificationError(f"cannot inspect metadata file {path}: {error}") from error
    if size > MAX_METADATA_BYTES:
        raise VerificationError(
            f"metadata file is too large: {size} bytes "
            f"(maximum {MAX_METADATA_BYTES})"
        )
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise VerificationError(f"cannot read metadata JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise VerificationError("metadata JSON root must be an object")
    return value


def verify_repository_policy(metadata: dict[str, Any]) -> None:
    if metadata.get("enabled") is not True:
        raise VerificationError("repository release immutability is not enabled")
    enforced_by_owner = metadata.get("enforced_by_owner")
    if not isinstance(enforced_by_owner, bool):
        raise VerificationError(
            "repository immutability metadata has no boolean enforced_by_owner"
        )


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
    except OSError as error:
        raise VerificationError(f"cannot hash release asset {path}: {error}") from error
    return digest.hexdigest()


def _validate_asset_name(name: str) -> None:
    if (
        not name
        or name in {".", ".."}
        or Path(name).name != name
        or "/" in name
        or "\\" in name
        or "\0" in name
    ):
        raise VerificationError(f"unsafe release asset name: {name!r}")


def collect_local_assets(
    asset_dir: Path,
    expected_names: list[str],
) -> dict[str, tuple[int, str]]:
    if not expected_names:
        raise VerificationError("at least one expected release asset is required")
    if len(set(expected_names)) != len(expected_names):
        raise VerificationError("expected release asset names contain duplicates")
    for name in expected_names:
        _validate_asset_name(name)

    try:
        entries = list(asset_dir.iterdir())
    except OSError as error:
        raise VerificationError(
            f"cannot list release asset directory {asset_dir}: {error}"
        ) from error

    actual_names: list[str] = []
    for entry in entries:
        try:
            mode = entry.lstat().st_mode
        except OSError as error:
            raise VerificationError(f"cannot inspect release asset {entry}: {error}") from error
        if not stat.S_ISREG(mode):
            raise VerificationError(f"release asset is not a regular file: {entry.name}")
        actual_names.append(entry.name)

    if sorted(actual_names) != sorted(expected_names):
        raise VerificationError(
            "local release assets differ from the expected set: "
            f"actual={sorted(actual_names)!r}, expected={sorted(expected_names)!r}"
        )

    assets: dict[str, tuple[int, str]] = {}
    for name in expected_names:
        path = asset_dir / name
        size = path.stat().st_size
        if size <= 0:
            raise VerificationError(f"release asset is empty: {name}")
        assets[name] = (size, file_sha256(path))
    return assets


def verify_checksum_file(
    asset_dir: Path,
    assets: dict[str, tuple[int, str]],
    checksum_name: str,
) -> None:
    if checksum_name not in assets:
        raise VerificationError(
            f"checksum file {checksum_name!r} is not an expected release asset"
        )
    archive_names = set(assets) - {checksum_name}
    try:
        lines = (asset_dir / checksum_name).read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise VerificationError(f"cannot read {checksum_name}: {error}") from error

    recorded: dict[str, str] = {}
    for line in lines:
        match = CHECKSUM_LINE_RE.fullmatch(line)
        if match is None:
            raise VerificationError(f"malformed {checksum_name} line: {line!r}")
        digest, name = match.groups()
        if name in recorded:
            raise VerificationError(f"duplicate {checksum_name} entry: {name}")
        recorded[name] = digest

    if set(recorded) != archive_names:
        raise VerificationError(
            f"{checksum_name} entries differ from release archives: "
            f"actual={sorted(recorded)!r}, expected={sorted(archive_names)!r}"
        )
    for name, digest in recorded.items():
        if digest != assets[name][1]:
            raise VerificationError(f"{checksum_name} digest mismatch for {name}")


def verify_release_metadata(
    metadata: dict[str, Any],
    *,
    tag: str,
    phase: str,
    assets: dict[str, tuple[int, str]],
) -> None:
    if metadata.get("tag_name") != tag:
        raise VerificationError(
            f"release tag mismatch: {metadata.get('tag_name')!r} != {tag!r}"
        )
    expected_draft = phase == "draft"
    if metadata.get("draft") is not expected_draft:
        raise VerificationError(
            f"release draft state does not match {phase!r} verification"
        )
    if metadata.get("prerelease") is not False:
        raise VerificationError("release must not be a prerelease")
    if phase == "published":
        if metadata.get("immutable") is not True:
            raise VerificationError("published release is not immutable yet")
        if not isinstance(metadata.get("published_at"), str):
            raise VerificationError("published release has no published_at timestamp")
    elif phase == "draft":
        if metadata.get("immutable") is not False:
            raise VerificationError("draft release has an unexpected immutable state")
        if metadata.get("published_at") is not None:
            raise VerificationError("draft release unexpectedly has published_at")
    else:
        raise VerificationError(f"unsupported release phase: {phase!r}")

    remote_assets = metadata.get("assets")
    if not isinstance(remote_assets, list):
        raise VerificationError("release metadata assets must be an array")

    by_name: dict[str, dict[str, Any]] = {}
    for item in remote_assets:
        if not isinstance(item, dict):
            raise VerificationError("release metadata contains a non-object asset")
        name = item.get("name")
        if not isinstance(name, str):
            raise VerificationError("release asset has no string name")
        _validate_asset_name(name)
        if name in by_name:
            raise VerificationError(f"duplicate GitHub release asset: {name}")
        by_name[name] = item

    if set(by_name) != set(assets):
        raise VerificationError(
            "GitHub release assets differ from the expected set: "
            f"actual={sorted(by_name)!r}, expected={sorted(assets)!r}"
        )

    for name, (local_size, local_digest) in assets.items():
        remote = by_name[name]
        if remote.get("state") != "uploaded":
            raise VerificationError(f"GitHub release asset is not uploaded: {name}")
        remote_size = remote.get("size")
        if (
            not isinstance(remote_size, int)
            or isinstance(remote_size, bool)
            or remote_size != local_size
        ):
            raise VerificationError(
                f"GitHub release asset size mismatch for {name}: "
                f"{remote_size!r} != {local_size}"
            )
        digest = remote.get("digest")
        if not isinstance(digest, str):
            raise VerificationError(f"GitHub release asset has no digest yet: {name}")
        match = SHA256_RE.fullmatch(digest)
        if match is None:
            raise VerificationError(
                f"GitHub release asset has no exact SHA-256 digest: {name}"
            )
        if match.group(1) != local_digest:
            raise VerificationError(
                f"GitHub SHA-256 digest does not match the local asset: {name}"
            )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    policy = subparsers.add_parser(
        "policy",
        help="verify repository immutable-release policy metadata",
    )
    policy.add_argument("--metadata", required=True, type=Path)

    release = subparsers.add_parser(
        "release",
        help="verify draft or published GitHub release metadata",
    )
    release.add_argument("--metadata", required=True, type=Path)
    release.add_argument("--tag", required=True)
    release.add_argument(
        "--phase",
        required=True,
        choices=("draft", "published"),
    )
    release.add_argument("--asset-dir", required=True, type=Path)
    release.add_argument(
        "--asset",
        required=True,
        action="append",
        dest="assets",
        help="expected asset basename; repeat for every asset",
    )
    release.add_argument(
        "--checksum-file",
        default="SHA256SUMS.txt",
        help="asset that records the other assets' SHA-256 digests",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        metadata = load_metadata(args.metadata)
        if args.command == "policy":
            verify_repository_policy(metadata)
        else:
            assets = collect_local_assets(args.asset_dir, args.assets)
            verify_checksum_file(args.asset_dir, assets, args.checksum_file)
            verify_release_metadata(
                metadata,
                tag=args.tag,
                phase=args.phase,
                assets=assets,
            )
    except VerificationError as error:
        print(f"release verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
