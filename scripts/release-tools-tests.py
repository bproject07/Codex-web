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
import stat
import struct
import tarfile
import tempfile
import unittest
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
        if path != "THIRD_PARTY_LICENSES/manifest.json"
    }
    files[f"{root}/{binary_name}"] = (
        fake_windows_binary()
        if platform == "windows"
        else fake_linux_binary()
    )
    files[f"{root}/web/assets/index-fixture.js"] = b"export {};\n"
    files[f"{root}/THIRD_PARTY_LICENSES/manifest.json"] = (
        json.dumps(
            {
                "schemaVersion": 1,
                "target": target,
                "packages": [{"name": "fixture"}],
            }
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
        for unsafe in ("../escape", "/absolute", r"root\windows", "root/./dot"):
            with self.subTest(unsafe=unsafe):
                with self.assertRaises(RuntimeError):
                    archives.canonical_member_name(unsafe)
        with self.assertRaises(RuntimeError):
            archives.validate_member_names(
                [("root/File", False), ("root/file", False)],
                "root",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
