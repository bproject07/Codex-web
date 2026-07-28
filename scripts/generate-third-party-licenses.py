#!/usr/bin/env python3
"""Generate a fail-closed third-party license bundle for one release target."""

from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
from typing import Any
from urllib.parse import urlsplit


ALLOWED_TARGETS = frozenset(
    {
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
    }
)
REVIEWED_LICENSE_EXPRESSIONS = frozenset(
    {
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "0BSD OR MIT OR Apache-2.0",
        "Apache-2.0 AND ISC",
        "Apache-2.0",
        "Apache-2.0 OR BSL-1.0",
        "Apache-2.0 OR ISC OR MIT",
        "Apache-2.0 OR MIT",
        "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
        "BSD-2-Clause OR Apache-2.0",
        "BSD-2-Clause OR Apache-2.0 OR MIT",
        "BSD-3-Clause",
        "CDLA-Permissive-2.0",
        "CC0-1.0 OR MIT-0 OR Apache-2.0",
        "ISC",
        "MIT",
        "MIT AND BSD-3-Clause",
        "MIT OR Apache-2.0",
        "MIT OR Apache-2.0 OR Zlib",
        "MIT OR Zlib OR Apache-2.0",
        "MPL-2.0",
        "Unicode-3.0",
        "Unlicense OR MIT",
        "Zlib OR Apache-2.0 OR MIT",
    }
)
LICENSE_NAME_PREFIXES = (
    "COPYING",
    "COPYRIGHT",
    "LICENCE",
    "LICENSE",
    "NOTICE",
)
NPM_MANIFEST_ONLY_LICENSE_DECLARATIONS = frozenset(
    {
        ("stackback", "0.0.2"),
    }
)
MAX_NOTICE_BYTES = 2 * 1024 * 1024
WHITESPACE_PATTERN = re.compile(r"\s+")


def parse_args() -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(
        description=(
            "Collect exact Rust and installed npm license/NOTICE files into a "
            "deterministic release bundle."
        )
    )
    parser.add_argument("--target", choices=sorted(ALLOWED_TARGETS), required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--repository", type=Path, default=repository)
    parser.add_argument(
        "--expected-rust-version",
        help="Fail unless rustc reports this exact release (for example, 1.95.0).",
    )
    return parser.parse_args()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_canonical_text_file(path: Path) -> str:
    data = path.read_bytes()
    if b"\0" in data:
        raise RuntimeError(f"lockfile is binary: {path}")
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError as error:
        raise RuntimeError(f"lockfile is not valid UTF-8: {path}") from error
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    return sha256_bytes(normalized.encode("utf-8"))


def validate_license_expression(package: str, expression: Any) -> str:
    if not isinstance(expression, str) or not expression.strip():
        raise RuntimeError(f"{package} has no declared license expression")
    normalized = WHITESPACE_PATTERN.sub(
        " ",
        expression.strip().replace("/", " OR "),
    )
    if normalized not in REVIEWED_LICENSE_EXPRESSIONS:
        raise RuntimeError(
            f"{package} introduces an unreviewed or malformed license "
            f"expression: {expression!r}"
        )
    return expression.strip()


def collect_notice_files(package: str, directory: Path) -> list[dict[str, str]]:
    if not directory.is_dir() or directory.is_symlink():
        raise RuntimeError(f"unsafe or missing package directory for {package}")

    paths = sorted(
        (
            path
            for path in directory.iterdir()
            if path.is_file()
            and not path.is_symlink()
            and path.name.upper().startswith(LICENSE_NAME_PREFIXES)
        ),
        key=lambda path: (path.name.casefold(), path.name),
    )
    if not paths:
        raise RuntimeError(f"{package} does not ship a top-level license/NOTICE file")

    files: list[dict[str, str]] = []
    for path in paths:
        data = path.read_bytes()
        if not data or len(data) > MAX_NOTICE_BYTES or b"\0" in data:
            raise RuntimeError(f"{package}/{path.name} is empty, binary, or too large")
        try:
            text = data.decode("utf-8-sig")
        except UnicodeDecodeError as error:
            raise RuntimeError(
                f"{package}/{path.name} is not valid UTF-8"
            ) from error
        normalized = text.replace("\r\n", "\n").replace("\r", "\n")
        files.append(
            {
                "path": path.name,
                "sha256": sha256_bytes(normalized.encode("utf-8")),
                "text": normalized,
            }
        )
    return files


def collect_exact_notice_file(
    package: str,
    root: Path,
    relative_path: str,
) -> dict[str, str]:
    root = root.resolve()
    candidate = root / Path(relative_path)
    path = candidate.resolve()
    if (
        root not in path.parents
        or not candidate.is_file()
        or candidate.is_symlink()
    ):
        raise RuntimeError(
            f"{package} is missing required regular file {relative_path}"
        )

    data = path.read_bytes()
    if not data or len(data) > MAX_NOTICE_BYTES or b"\0" in data:
        raise RuntimeError(
            f"{package}/{relative_path} is empty, binary, or too large"
        )
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError as error:
        raise RuntimeError(
            f"{package}/{relative_path} is not valid UTF-8"
        ) from error
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    return {
        "path": relative_path,
        "sha256": sha256_bytes(normalized.encode("utf-8")),
        "text": normalized,
    }


def npm_repository_url(manifest: dict[str, Any]) -> str | None:
    repository = manifest.get("repository")
    if isinstance(repository, dict):
        repository = repository.get("url")
    return repository if isinstance(repository, str) and repository else None


def npm_name_from_lock_path(relative: str) -> str:
    if "\\" in relative:
        raise RuntimeError(f"invalid npm lockfile package path: {relative}")
    parts = relative.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise RuntimeError(f"invalid npm lockfile package path: {relative}")
    try:
        node_modules_index = len(parts) - 1 - parts[::-1].index("node_modules")
    except ValueError as error:
        raise RuntimeError(f"invalid npm lockfile package path: {relative}") from error
    package_parts = parts[node_modules_index + 1 :]
    if (
        len(package_parts) == 1
        and package_parts[0]
        and not package_parts[0].startswith("@")
    ):
        return package_parts[0]
    if (
        len(package_parts) == 2
        and package_parts[0].startswith("@")
        and package_parts[0] != "@"
        and package_parts[1]
    ):
        return "/".join(package_parts)
    raise RuntimeError(f"invalid npm lockfile package path: {relative}")


def validate_npm_lock_provenance(
    identity: str,
    locked: dict[str, Any],
) -> tuple[str, str]:
    resolved = locked.get("resolved")
    integrity = locked.get("integrity")
    if not isinstance(resolved, str):
        raise RuntimeError(f"{identity} has no locked registry tarball URL")
    parsed = urlsplit(resolved)
    if (
        parsed.scheme != "https"
        or parsed.hostname != "registry.npmjs.org"
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or not parsed.path.endswith(".tgz")
    ):
        raise RuntimeError(f"{identity} has an unreviewed registry URL: {resolved!r}")
    if not isinstance(integrity, str) or not integrity.startswith("sha512-"):
        raise RuntimeError(f"{identity} has no reviewed SHA-512 lock integrity")
    try:
        digest = base64.b64decode(integrity.removeprefix("sha512-"), validate=True)
    except (ValueError, binascii.Error) as error:
        raise RuntimeError(f"{identity} has malformed lock integrity") from error
    if len(digest) != 64:
        raise RuntimeError(f"{identity} has malformed SHA-512 lock integrity")
    return resolved, integrity


def collect_npm_license_evidence(
    identity: str,
    name: str,
    version: str,
    declared_license: str,
    role: str,
    repository_url: str | None,
    web: Path,
    relative: str,
    directory: Path,
) -> tuple[list[dict[str, str]], str, str]:
    matching_entries = [
        path
        for path in directory.iterdir()
        if path.name.upper().startswith(LICENSE_NAME_PREFIXES)
    ]
    if matching_entries:
        return (
            collect_notice_files(identity, directory),
            "installed-license-notice-files",
            relative,
        )

    if role != "build":
        raise RuntimeError(
            f"runtime npm package {identity} does not ship a top-level "
            "license/NOTICE file"
        )

    if name.startswith("@rolldown/binding-"):
        provider_directory = (web / "node_modules" / "rolldown").resolve()
        provider_manifest_path = provider_directory / "package.json"
        if (
            not provider_manifest_path.is_file()
            or provider_manifest_path.is_symlink()
        ):
            raise RuntimeError(
                f"{identity} requires the installed same-version rolldown "
                "package for its license attribution"
            )
        provider_manifest = json.loads(
            provider_manifest_path.read_text(encoding="utf-8")
        )
        provider_name = provider_manifest.get("name")
        provider_version = provider_manifest.get("version")
        provider_license = validate_license_expression(
            f"rolldown {provider_version}",
            provider_manifest.get("license"),
        )
        provider_repository = npm_repository_url(provider_manifest)
        if (
            provider_name != "rolldown"
            or provider_version != version
            or provider_license != declared_license
            or provider_repository != repository_url
        ):
            raise RuntimeError(
                f"{identity} does not match its installed rolldown license "
                "provider"
            )
        provider_files = collect_notice_files(
            f"rolldown {provider_version}",
            provider_directory,
        )
        for item in provider_files:
            item["path"] = f"node_modules/rolldown/{item['path']}"
        return (
            provider_files,
            "related-installed-package-license",
            f"node_modules/rolldown@{provider_version}",
        )

    if (name, version) not in NPM_MANIFEST_ONLY_LICENSE_DECLARATIONS:
        raise RuntimeError(
            f"build npm package {identity} has no reviewed license/NOTICE "
            "attribution source"
        )

    # This reviewed legacy build-only package ships an SPDX declaration but no
    # license file. Preserve its exact installed package manifest rather than
    # downloading mutable text during a release build.
    return (
        [collect_exact_notice_file(identity, directory, "package.json")],
        "package-manifest-license-declaration",
        f"{relative}/package.json",
    )


def rust_toolchain_package(
    expected_version: str | None,
) -> tuple[dict[str, str], dict[str, Any]]:
    version_result = subprocess.run(
        ["rustc", "--version", "--verbose"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    fields: dict[str, str] = {}
    for line in version_result.stdout.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            fields[key.strip()] = value.strip()

    release = fields.get("release")
    host = fields.get("host")
    if not release or not host:
        raise RuntimeError("rustc --version --verbose did not report release and host")
    if expected_version is not None and release != expected_version:
        raise RuntimeError(
            f"rustc release {release!r} does not match expected "
            f"{expected_version!r}"
        )

    sysroot_result = subprocess.run(
        ["rustc", "--print", "sysroot"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    sysroot_text = sysroot_result.stdout.strip()
    if not sysroot_text:
        raise RuntimeError("rustc --print sysroot returned an empty path")
    sysroot = Path(sysroot_text)
    if not sysroot.is_absolute() or not sysroot.is_dir():
        raise RuntimeError(f"rustc reported an invalid sysroot: {sysroot_text!r}")

    identity = f"Rust standard library {release}"
    notice_path = "share/doc/rust/COPYRIGHT-library.html"
    package = {
        "ecosystem": "rust-toolchain",
        "name": "rust-standard-library",
        "version": release,
        "license": validate_license_expression(identity, "Apache-2.0 OR MIT"),
        "source": "https://github.com/rust-lang/rust",
        "files": [
            collect_exact_notice_file(identity, sysroot, notice_path),
        ],
    }
    return {"release": release, "host": host}, package


def cargo_metadata(server: Path, target: str) -> dict[str, Any]:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            target,
        ],
        cwd=server,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    return json.loads(result.stdout)


def rust_runtime_packages(server: Path, target: str) -> list[dict[str, Any]]:
    metadata = cargo_metadata(server, target)
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("root"), str):
        raise RuntimeError("cargo metadata did not identify the workspace root")

    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in resolve["nodes"]}
    root_id = resolve["root"]
    selected: set[str] = set()
    pending = [root_id]
    while pending:
        package_id = pending.pop()
        if package_id in selected:
            continue
        selected.add(package_id)
        for dependency in nodes[package_id].get("deps", []):
            dependency_kinds = dependency.get("dep_kinds") or [{"kind": None}]
            if any(kind.get("kind") != "dev" for kind in dependency_kinds):
                pending.append(dependency["pkg"])

    output: list[dict[str, Any]] = []
    for package_id in selected:
        if package_id == root_id:
            continue
        package = packages[package_id]
        identity = f"{package['name']} {package['version']}"
        source = package.get("source")
        if not isinstance(source, str) or not source.startswith("registry+"):
            raise RuntimeError(f"{identity} does not come from a locked Cargo registry")
        directory = Path(package["manifest_path"]).resolve().parent
        files = collect_notice_files(identity, directory)
        output.append(
            {
                "ecosystem": "cargo",
                "name": package["name"],
                "version": package["version"],
                "license": validate_license_expression(identity, package.get("license")),
                "source": source,
                "files": files,
            }
        )

    output.sort(key=lambda package: (package["name"], package["version"]))
    validate_special_rust_notices(output)
    return output


def validate_special_rust_notices(packages: list[dict[str, Any]]) -> None:
    by_name = {package["name"]: package for package in packages}
    required = {
        "atomic-waker": "LICENSE-THIRD-PARTY",
        "matchit": "LICENSE.httprouter",
        "unicode-ident": "LICENSE-UNICODE",
    }
    for package_name, required_file in required.items():
        package = by_name.get(package_name)
        if package is None:
            continue
        names = {item["path"] for item in package["files"]}
        if required_file not in names:
            raise RuntimeError(
                f"{package_name} is missing required attribution {required_file}"
            )

    for package in packages:
        if package["name"].startswith("icu_") and not any(
            item["path"].upper().startswith("LICENSE")
            for item in package["files"]
        ):
            raise RuntimeError(
                f"{package['name']} is missing its Unicode/IBM license text"
            )


def npm_installed_packages(web: Path) -> list[dict[str, Any]]:
    lock_path = web / "package-lock.json"
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    package_entries = lock.get("packages")
    if not isinstance(package_entries, dict):
        raise RuntimeError("package-lock.json does not contain a packages inventory")
    web_manifest = json.loads((web / "package.json").read_text(encoding="utf-8"))
    root_entry = package_entries.get("")
    if (
        not isinstance(root_entry, dict)
        or root_entry.get("name") != web_manifest.get("name")
        or root_entry.get("version") != web_manifest.get("version")
    ):
        raise RuntimeError(
            "package-lock.json root name/version does not match web/package.json"
        )

    output: list[dict[str, Any]] = []
    for relative, locked in sorted(package_entries.items()):
        if not relative.startswith("node_modules/"):
            continue
        if not isinstance(locked, dict):
            raise RuntimeError(f"invalid npm lockfile package entry: {relative}")
        expected_name = npm_name_from_lock_path(relative)
        locked_version = locked.get("version")
        identity = f"{expected_name} {locked_version}"
        resolved, integrity = validate_npm_lock_provenance(identity, locked)
        directory = (web / Path(relative)).resolve()
        node_modules = (web / "node_modules").resolve()
        if node_modules not in directory.parents:
            raise RuntimeError(f"unsafe npm package path: {relative}")
        manifest_path = directory / "package.json"
        if not manifest_path.is_file() or manifest_path.is_symlink():
            if locked.get("optional") is True and not directory.exists():
                continue
            raise RuntimeError(f"locked npm package is not installed: {relative}")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        name = manifest.get("name")
        version = manifest.get("version")
        if (
            not isinstance(name, str)
            or not isinstance(version, str)
            or name != expected_name
            or version != locked_version
        ):
            raise RuntimeError(f"installed npm metadata does not match lockfile: {relative}")
        identity = f"{name} {version}"
        repository = npm_repository_url(manifest)
        source = (
            repository
            if isinstance(repository, str) and repository
            else f"https://www.npmjs.com/package/{name}/v/{version}"
        )
        declared_license = validate_license_expression(
            identity,
            manifest.get("license"),
        )
        role = "build" if locked.get("dev") is True else "runtime"
        files, license_evidence, attribution_source = collect_npm_license_evidence(
            identity,
            name,
            version,
            declared_license,
            role,
            repository,
            web,
            relative,
            directory,
        )
        output.append(
            {
                "ecosystem": "npm",
                "name": name,
                "version": version,
                "license": declared_license,
                "source": source,
                "resolved": resolved,
                "integrity": integrity,
                "role": role,
                "licenseEvidence": license_evidence,
                "attributionSource": attribution_source,
                "files": files,
            }
        )

    if not output:
        raise RuntimeError("no installed npm packages were found")
    output.sort(key=lambda package: (package["name"], package["version"]))
    return output


def render_text(
    target: str,
    rustc: dict[str, str],
    lock_hashes: dict[str, str],
    packages: list[dict[str, Any]],
) -> str:
    lines = [
        "THIRD-PARTY LICENSES AND NOTICES",
        "",
        "This bundle accompanies the compiled Codex Web Terminal distribution.",
        "Third-party packages remain subject to their own license terms.",
        "",
        f"Release target: {target}",
        f"rustc release: {rustc['release']}",
        f"rustc host: {rustc['host']}",
        (
            "server/Cargo.lock canonical UTF-8/LF SHA-256: "
            f"{lock_hashes['server/Cargo.lock']}"
        ),
        (
            "web/package-lock.json canonical UTF-8/LF SHA-256: "
            f"{lock_hashes['web/package-lock.json']}"
        ),
        "",
    ]
    for package in packages:
        lines.extend(
            [
                "=" * 78,
                (
                    f"{package['ecosystem']}: {package['name']} "
                    f"{package['version']}"
                ),
                f"Declared license: {package['license']}",
                f"Source: {package['source']}",
            ]
        )
        if package["ecosystem"] == "npm":
            lines.extend(
                [
                    f"Package role: {package['role']}",
                    f"Locked tarball: {package['resolved']}",
                    f"Lock integrity: {package['integrity']}",
                    f"License evidence: {package['licenseEvidence']}",
                    f"Attribution source: {package['attributionSource']}",
                ]
            )
        lines.append("")
        if package.get("licenseEvidence") == "package-manifest-license-declaration":
            lines.extend(
                [
                    (
                        "No top-level license/NOTICE file was installed; the "
                        "exact package.json license declaration follows."
                    ),
                    "",
                ]
            )
        for item in package["files"]:
            lines.extend(
                [
                    f"--- {item['path']} ---",
                    item["text"].rstrip("\n"),
                    "",
                ]
            )
    return "\n".join(lines).rstrip() + "\n"


def write_bundle(
    output_dir: Path,
    target: str,
    rustc: dict[str, str],
    lock_hashes: dict[str, str],
    packages: list[dict[str, Any]],
) -> None:
    if output_dir.exists() or output_dir.is_symlink():
        raise RuntimeError(f"refusing to replace existing output: {output_dir}")
    output_parent = output_dir.parent.resolve()
    output_parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(
        tempfile.mkdtemp(prefix=".third-party-licenses-", dir=output_parent)
    )
    try:
        license_text = render_text(target, rustc, lock_hashes, packages)
        (temporary / "THIRD_PARTY_LICENSES.txt").write_text(
            license_text, encoding="utf-8", newline="\n"
        )
        manifest_packages = []
        for package in packages:
            manifest_packages.append(
                {
                    "ecosystem": package["ecosystem"],
                    "name": package["name"],
                    "version": package["version"],
                    "license": package["license"],
                    "source": package["source"],
                    **({"role": package["role"]} if "role" in package else {}),
                    **(
                        {"resolved": package["resolved"]}
                        if "resolved" in package
                        else {}
                    ),
                    **(
                        {"integrity": package["integrity"]}
                        if "integrity" in package
                        else {}
                    ),
                    **(
                        {"licenseEvidence": package["licenseEvidence"]}
                        if "licenseEvidence" in package
                        else {}
                    ),
                    **(
                        {"attributionSource": package["attributionSource"]}
                        if "attributionSource" in package
                        else {}
                    ),
                    "files": [
                        {"path": item["path"], "sha256": item["sha256"]}
                        for item in package["files"]
                    ],
                }
            )
        manifest = {
            "schemaVersion": 1,
            "target": target,
            "rustc": rustc,
            "lockfileHashFormat": "sha256-utf8-lf",
            "lockfiles": lock_hashes,
            "packages": manifest_packages,
        }
        (temporary / "manifest.json").write_text(
            json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
            newline="\n",
        )
        temporary.chmod(0o755)
        os.replace(temporary, output_dir)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def main() -> int:
    args = parse_args()
    repository = args.repository.resolve()
    server = repository / "server"
    web = repository / "web"
    cargo_lock = server / "Cargo.lock"
    npm_lock = web / "package-lock.json"
    if not cargo_lock.is_file() or not npm_lock.is_file():
        raise RuntimeError("repository lockfiles are missing")

    rustc, toolchain_package = rust_toolchain_package(args.expected_rust_version)
    packages = rust_runtime_packages(server, args.target)
    packages.extend(npm_installed_packages(web))
    packages.append(toolchain_package)
    packages.sort(
        key=lambda package: (
            package["ecosystem"],
            package["name"],
            package["version"],
        )
    )
    lock_hashes = {
        "server/Cargo.lock": sha256_canonical_text_file(cargo_lock),
        "web/package-lock.json": sha256_canonical_text_file(npm_lock),
    }
    write_bundle(
        args.output_dir.resolve(),
        args.target,
        rustc,
        lock_hashes,
        packages,
    )
    print(
        f"Generated {len(packages)} package notices in "
        f"{args.output_dir.resolve()}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
