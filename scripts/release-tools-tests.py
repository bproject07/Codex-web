#!/usr/bin/env python3
"""Focused standard-library tests for fail-closed release tooling."""

from __future__ import annotations

import argparse
import base64
from io import BytesIO
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import struct
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile


SCRIPTS = Path(__file__).resolve().parent


def load_script(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {filename}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


licenses = load_script(
    "codex_web_license_generator",
    "generate-third-party-licenses.py",
)
archives = load_script(
    "codex_web_archive_validator",
    "validate-release-archive.py",
)
package_markers = load_script(
    "codex_web_release_package_manifest",
    "generate-release-package-manifest.py",
)
release_metadata = load_script(
    "codex_web_github_release_verifier",
    "verify-github-release.py",
)


def fake_windows_binary() -> bytes:
    data = bytearray(256)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<H", data, 0x84, 0x8664)
    return bytes(data)


def fake_linux_binary() -> bytes:
    data = bytearray(64)
    data[:4] = b"\x7fELF"
    data[4] = 2
    data[5] = 1
    struct.pack_into("<H", data, 18, 62)
    return bytes(data)


def archive_files(root: str, platform: str) -> dict[str, bytes]:
    binary_name = "codex-web.exe" if platform == "windows" else "codex-web"
    target = (
        "x86_64-pc-windows-msvc"
        if platform == "windows"
        else "x86_64-unknown-linux-gnu"
    )
    files = {
        f"{root}/{path}": b"fixture\n"
        for path in archives.COMMON_REQUIRED_FILES
        if path not in {
            "THIRD_PARTY_LICENSES/manifest.json",
            "web/index.html",
        }
    }
    files[f"{root}/{binary_name}"] = (
        fake_windows_binary()
        if platform == "windows"
        else fake_linux_binary()
    )
    files[f"{root}/web/index.html"] = (
        b'<!doctype html><script type="module" '
        b'src="/assets/index-Cq2D_58X.js"></script>'
        b'<link rel="stylesheet" href="/assets/index-By1AeVe3.css">\n'
    )
    files[f"{root}/web/assets/index-Cq2D_58X.js"] = b"export {};\n"
    files[f"{root}/web/assets/index-By1AeVe3.css"] = b"body{}\n"
    files[f"{root}/THIRD_PARTY_LICENSES/manifest.json"] = (
        json.dumps(
            {
                "schemaVersion": 1,
                "target": target,
                "packages": [{"name": "fixture"}],
            }
        ).encode("utf-8")
    )
    version_match = re.search(r"-v([0-9]+\.[0-9]+\.[0-9]+)-", root)
    if version_match is None:
        raise RuntimeError(f"fixture root has no version: {root}")
    files[f"{root}/release-package.json"] = (
        json.dumps(
            package_markers.build_manifest(version_match.group(1), target)
        ).encode("utf-8")
    )
    return files


class LicenseGeneratorTests(unittest.TestCase):
    def test_license_expression_allowlist_rejects_malformed_values(self) -> None:
        self.assertEqual(
            licenses.validate_license_expression("fixture", "MIT OR Apache-2.0"),
            "MIT OR Apache-2.0",
        )
        for malformed in ("()", "OR", "MIT OR", "MIT Apache-2.0"):
            with self.subTest(malformed=malformed):
                with self.assertRaises(RuntimeError):
                    licenses.validate_license_expression("fixture", malformed)

    def test_npm_name_is_bound_to_its_lock_path(self) -> None:
        self.assertEqual(
            licenses.npm_name_from_lock_path("node_modules/react"),
            "react",
        )
        self.assertEqual(
            licenses.npm_name_from_lock_path(
                "node_modules/a/node_modules/@scope/package"
            ),
            "@scope/package",
        )
        for unsafe in (
            "node_modules/../react",
            r"node_modules\react",
            "node_modules/@scope",
        ):
            with self.subTest(unsafe=unsafe):
                with self.assertRaises(RuntimeError):
                    licenses.npm_name_from_lock_path(unsafe)

    def test_npm_registry_provenance_requires_https_and_sha512(self) -> None:
        digest = base64.b64encode(bytes(64)).decode("ascii")
        resolved, integrity = licenses.validate_npm_lock_provenance(
            "fixture 1.0.0",
            {
                "resolved": "https://registry.npmjs.org/fixture/-/fixture-1.0.0.tgz",
                "integrity": f"sha512-{digest}",
            },
        )
        self.assertTrue(resolved.endswith(".tgz"))
        self.assertTrue(integrity.startswith("sha512-"))
        for invalid in (
            {
                "resolved": "http://registry.npmjs.org/fixture/-/fixture.tgz",
                "integrity": f"sha512-{digest}",
            },
            {
                "resolved": "https://example.invalid/fixture.tgz",
                "integrity": f"sha512-{digest}",
            },
            {
                "resolved": "https://registry.npmjs.org/fixture/-/fixture.tgz",
                "integrity": "sha512-invalid",
            },
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(RuntimeError):
                    licenses.validate_npm_lock_provenance("fixture", invalid)

    def test_lock_hash_normalizes_line_endings(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            left = root / "left.lock"
            right = root / "right.lock"
            left.write_bytes(b"one\r\ntwo\r\n")
            right.write_bytes(b"one\ntwo\n")
            self.assertEqual(
                licenses.sha256_canonical_text_file(left),
                licenses.sha256_canonical_text_file(right),
            )

    @unittest.skipUnless(os.name == "posix", "POSIX directory mode check")
    def test_bundle_directory_is_shared_readable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "THIRD_PARTY_LICENSES"
            licenses.write_bundle(
                output,
                "x86_64-unknown-linux-gnu",
                {"release": "1.95.0", "host": "x86_64-unknown-linux-gnu"},
                {
                    "server/Cargo.lock": "cargo-hash",
                    "web/package-lock.json": "npm-hash",
                },
                [],
            )
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o755)


class ReleasePackageManifestTests(unittest.TestCase):
    def test_manifest_is_exact_and_stable(self) -> None:
        self.assertEqual(
            package_markers.build_manifest(
                "1.2.3",
                "x86_64-pc-windows-msvc",
            ),
            {
                "schemaVersion": 1,
                "product": "codex-web-terminal",
                "version": "1.2.3",
                "target": "x86_64-pc-windows-msvc",
            },
        )
        for invalid in ("v1.2.3", "1.2", "1.2.3-beta.1", "../1.2.3"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(RuntimeError):
                    package_markers.build_manifest(
                        invalid,
                        "x86_64-pc-windows-msvc",
                    )

    def test_marker_requires_a_complete_package_and_never_overwrites(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "web").mkdir()
            (root / "web" / "index.html").write_text("web", encoding="utf-8")
            (root / "THIRD_PARTY_LICENSES").mkdir()
            (root / "THIRD_PARTY_LICENSES" / "manifest.json").write_text(
                "{}",
                encoding="utf-8",
            )
            (root / "codex-web.exe").write_bytes(fake_windows_binary())

            marker = package_markers.write_manifest(
                root,
                "1.2.3",
                "x86_64-pc-windows-msvc",
            )
            self.assertTrue(marker.is_file())
            with self.assertRaises(RuntimeError):
                package_markers.write_manifest(
                    root,
                    "1.2.3",
                    "x86_64-pc-windows-msvc",
                )


class GitHubReleaseVerifierTests(unittest.TestCase):
    def _fixture(
        self,
        root: Path,
        *,
        phase: str,
    ) -> tuple[dict[str, object], dict[str, tuple[int, str]]]:
        windows = root / "codex-web-terminal-v1.2.3-windows-x86_64.zip"
        linux = root / "codex-web-terminal-v1.2.3-linux-x86_64-glibc.tar.gz"
        windows.write_bytes(b"windows archive")
        linux.write_bytes(b"linux archive")
        windows_digest = release_metadata.file_sha256(windows)
        linux_digest = release_metadata.file_sha256(linux)
        (root / "SHA256SUMS.txt").write_text(
            f"{windows_digest}  {windows.name}\n"
            f"{linux_digest}  {linux.name}\n",
            encoding="utf-8",
        )
        names = [windows.name, linux.name, "SHA256SUMS.txt"]
        assets = release_metadata.collect_local_assets(root, names)
        metadata: dict[str, object] = {
            "tag_name": "v1.2.3",
            "draft": phase == "draft",
            "prerelease": False,
            "immutable": phase == "published",
            "published_at": (
                "2026-07-28T12:00:00Z" if phase == "published" else None
            ),
            "assets": [
                {
                    "name": name,
                    "state": "uploaded",
                    "size": assets[name][0],
                    "digest": f"sha256:{assets[name][1]}",
                }
                for name in names
            ],
        }
        return metadata, assets

    def test_repository_policy_must_be_explicitly_enabled(self) -> None:
        release_metadata.verify_repository_policy(
            {"enabled": True, "enforced_by_owner": False}
        )
        for invalid in (
            {"enabled": False, "enforced_by_owner": False},
            {"enabled": True},
            {"enabled": True, "enforced_by_owner": "false"},
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(release_metadata.VerificationError):
                    release_metadata.verify_repository_policy(invalid)

    def test_draft_and_published_metadata_bind_every_local_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for phase in ("draft", "published"):
                with self.subTest(phase=phase):
                    metadata, assets = self._fixture(root, phase=phase)
                    release_metadata.verify_checksum_file(
                        root,
                        assets,
                        "SHA256SUMS.txt",
                    )
                    release_metadata.verify_release_metadata(
                        metadata,
                        tag="v1.2.3",
                        phase=phase,
                        assets=assets,
                    )

    def test_published_metadata_rejects_mutability_and_missing_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            metadata, assets = self._fixture(
                Path(temporary),
                phase="published",
            )
            metadata["immutable"] = False
            with self.assertRaises(release_metadata.VerificationError):
                release_metadata.verify_release_metadata(
                    metadata,
                    tag="v1.2.3",
                    phase="published",
                    assets=assets,
                )

            metadata["immutable"] = True
            metadata_assets = metadata["assets"]
            assert isinstance(metadata_assets, list)
            assert isinstance(metadata_assets[0], dict)
            metadata_assets[0]["digest"] = None
            with self.assertRaises(release_metadata.VerificationError):
                release_metadata.verify_release_metadata(
                    metadata,
                    tag="v1.2.3",
                    phase="published",
                    assets=assets,
                )

    def test_checksum_file_rejects_mismatched_archive_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, assets = self._fixture(root, phase="draft")
            checksum = root / "SHA256SUMS.txt"
            checksum.write_text(
                checksum.read_text(encoding="utf-8").replace(
                    assets[
                        "codex-web-terminal-v1.2.3-windows-x86_64.zip"
                    ][1],
                    "0" * 64,
                    1,
                ),
                encoding="utf-8",
            )
            with self.assertRaises(release_metadata.VerificationError):
                release_metadata.verify_checksum_file(
                    root,
                    assets,
                    "SHA256SUMS.txt",
                )


class ArchiveValidatorTests(unittest.TestCase):
    def test_windows_zip_has_one_safe_complete_root(self) -> None:
        root_name = "codex-web-terminal-v0.1.0-windows-x86_64"
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / f"{root_name}.zip"
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as package:
                for name, data in archive_files(root_name, "windows").items():
                    package.writestr(name, data)
            archives.validate_archive(
                argparse.Namespace(
                    archive=archive,
                    expected_root=root_name,
                    platform="windows",
                    expected_version="0.1.0",
                    execute_smoke=False,
                )
            )

    def test_linux_tar_has_one_safe_complete_root(self) -> None:
        root_name = "codex-web-terminal-v0.1.0-linux-x86_64-glibc"
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / f"{root_name}.tar.gz"
            with tarfile.open(archive, "w:gz") as package:
                for name, data in archive_files(root_name, "linux").items():
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    info.mode = 0o755 if name.endswith("/codex-web") else 0o644
                    package.addfile(info, BytesIO(data))
            archives.validate_archive(
                argparse.Namespace(
                    archive=archive,
                    expected_root=root_name,
                    platform="linux",
                    expected_version="0.1.0",
                    execute_smoke=False,
                )
            )

    def test_archive_paths_reject_traversal_and_case_collisions(self) -> None:
        for unsafe in (
            "../escape",
            "/absolute",
            r"root\windows",
            "root/./dot",
            "root/web/index.html:stream",
            "root/web/trailing.",
            "root/web/NUL.txt",
            "root/web/COM¹.log",
            "root/web/control\x7f",
            f"root/{'x' * archives.MAX_ARCHIVE_PATH_BYTES}",
        ):
            with self.subTest(unsafe=unsafe):
                with self.assertRaises(RuntimeError):
                    archives.canonical_member_name(unsafe)
        with self.assertRaises(RuntimeError):
            archives.validate_member_names(
                [("root/File", False), ("root/file", False)],
                "root",
            )

    def test_frontend_validation_binds_every_hashed_index_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "web" / "assets"
            assets.mkdir(parents=True)
            (root / "web" / "index.html").write_text(
                '<script src="/assets/index-Cq2D_58X.js"></script>'
                '<link href="/assets/index-By1AeVe3.css" rel="stylesheet">',
                encoding="utf-8",
            )
            (assets / "index-Cq2D_58X.js").write_text(
                "export {};\n",
                encoding="utf-8",
            )
            stylesheet = assets / "index-By1AeVe3.css"
            stylesheet.write_text("body{}\n", encoding="utf-8")

            archives.validate_frontend_assets(root)
            stylesheet.unlink()
            with self.assertRaises(RuntimeError):
                archives.validate_frontend_assets(root)
            stylesheet.write_text("body{}\n", encoding="utf-8")

            (root / "web" / "index.html").write_bytes(b"\xff")
            with self.assertRaises(RuntimeError):
                archives.validate_frontend_assets(root)
            (root / "web" / "index.html").write_bytes(
                b"x" * (archives.MAX_INDEX_BYTES + 1)
            )
            with self.assertRaises(RuntimeError):
                archives.validate_frontend_assets(root)

    def test_zip_resource_limits_are_fail_closed(self) -> None:
        root_name = "codex-web-terminal-v0.1.0-windows-x86_64"
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            archive = directory / f"{root_name}.zip"
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as package:
                for name, data in archive_files(root_name, "windows").items():
                    package.writestr(name, data)
                package.writestr(
                    f"{root_name}/web/assets/bomb-Cq2D_58X.js",
                    b"A" * 10_000,
                )

            with mock.patch.object(archives, "MAX_ARCHIVE_ENTRIES", 1):
                with self.assertRaises(RuntimeError):
                    archives.inspect_and_extract_zip(
                        archive,
                        directory / "entries",
                        root_name,
                        "windows",
                    )
            with mock.patch.object(archives, "MAX_EXTRACTED_BYTES", 1):
                with self.assertRaises(RuntimeError):
                    archives.inspect_and_extract_zip(
                        archive,
                        directory / "bytes",
                        root_name,
                        "windows",
                    )
            with (
                mock.patch.object(archives, "ZIP_RATIO_MINIMUM_BYTES", 1),
                mock.patch.object(archives, "ZIP_RATIO_LIMIT", 1),
            ):
                with self.assertRaises(RuntimeError):
                    archives.inspect_and_extract_zip(
                        archive,
                        directory / "ratio",
                        root_name,
                        "windows",
                    )

    def test_tar_links_and_resource_limits_are_fail_closed(self) -> None:
        root_name = "codex-web-terminal-v0.1.0-linux-x86_64-glibc"
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            archive = directory / f"{root_name}.tar.gz"
            with tarfile.open(archive, "w:gz") as package:
                link = tarfile.TarInfo(f"{root_name}/web/link")
                link.type = tarfile.SYMTYPE
                link.linkname = "../../escape"
                package.addfile(link)
            with self.assertRaises(RuntimeError):
                archives.inspect_and_extract_tar(
                    archive,
                    directory / "link",
                    root_name,
                    "linux",
                )

            archive = directory / "bounded.tar.gz"
            with tarfile.open(archive, "w:gz") as package:
                for name, data in archive_files(root_name, "linux").items():
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    package.addfile(info, BytesIO(data))
            with mock.patch.object(archives, "MAX_EXTRACTED_BYTES", 1):
                with self.assertRaises(RuntimeError):
                    archives.inspect_and_extract_tar(
                        archive,
                        directory / "bytes",
                        root_name,
                        "linux",
                    )


if __name__ == "__main__":
    unittest.main(verbosity=2)
