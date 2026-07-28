#!/usr/bin/env python3
"""Deterministic native regression for sequential updater supervision."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid


FIXTURE_EXAMPLE = "updater-supervisor-fixture"
FIXTURE_TOKEN = "synthetic-supervisor-regression-token"
CONTROL_DIRECTORY = ".updater-supervisor-regression"
CONTROL_FILE = "control.json"
LIVE_PORT = 8789


class RegressionFailure(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fixture",
        type=Path,
        help="prebuilt updater-supervisor-fixture executable; skips cargo build",
    )
    parser.add_argument(
        "--root-server",
        type=Path,
        help=(
            "packaged codex-web executable to keep as the stable root; "
            "the synthetic fixture remains the supervised candidate"
        ),
    )
    parser.add_argument("--cargo", default="cargo", help="cargo executable")
    parser.add_argument(
        "--toolchain",
        help="optional rustup toolchain, for example 1.95.0-x86_64-pc-windows-gnu",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=0,
        help="disposable loopback port; 0 selects an available port",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=15.0,
        help="seconds allowed for each generation transition",
    )
    return parser.parse_args()


def cargo_command(cargo: str, toolchain: str | None, *arguments: str) -> list[str]:
    command = [cargo]
    if toolchain:
        command.append(f"+{toolchain}")
    command.extend(arguments)
    return command


def build_fixture(
    server_directory: Path, cargo: str, toolchain: str | None
) -> Path:
    subprocess.run(
        cargo_command(
            cargo,
            toolchain,
            "build",
            "--locked",
            "--example",
            FIXTURE_EXAMPLE,
        ),
        cwd=server_directory,
        check=True,
    )
    metadata = subprocess.run(
        cargo_command(cargo, toolchain, "metadata", "--no-deps", "--format-version=1"),
        cwd=server_directory,
        check=True,
        capture_output=True,
        text=True,
    )
    target_directory = Path(json.loads(metadata.stdout)["target_directory"])
    executable_name = FIXTURE_EXAMPLE + (".exe" if os.name == "nt" else "")
    executable = target_directory / "debug" / "examples" / executable_name
    if not executable.is_file():
        raise RegressionFailure(f"fixture executable was not built: {executable}")
    return executable.resolve()


def executable_version(executable: Path, label: str) -> tuple[int, int, int]:
    if not executable.is_file():
        raise RegressionFailure(f"{label} executable does not exist: {executable}")
    if os.name != "nt" and not os.access(executable, os.X_OK):
        raise RegressionFailure(f"{label} executable is not executable: {executable}")
    try:
        completed = subprocess.run(
            [str(executable), "--version"],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except subprocess.TimeoutExpired as error:
        raise RegressionFailure(f"{label} --version timed out") from error
    except subprocess.CalledProcessError as error:
        raise RegressionFailure(
            f"{label} --version exited with status {error.returncode}"
        ) from error

    reported = completed.stdout.strip()
    match = re.fullmatch(r"codex-web ([0-9]+)\.([0-9]+)\.([0-9]+)", reported)
    if match is None:
        raise RegressionFailure(
            f"{label} requires a stable numeric codex-web version, got {reported!r}"
        )
    return tuple(int(component) for component in match.groups())  # type: ignore[return-value]


def validate_root_server(executable: Path, expected_version: str) -> None:
    reported_version = version_text(executable_version(executable, "root server"))
    if reported_version != expected_version:
        raise RegressionFailure(
            "root server version mismatch: "
            f"expected {expected_version!r}, got {reported_version!r}"
        )


def version_text(version: tuple[int, int, int]) -> str:
    return ".".join(str(component) for component in version)


def next_versions(
    current: tuple[int, int, int],
) -> tuple[tuple[int, int, int], tuple[int, int, int], tuple[int, int, int]]:
    major, minor, patch = current
    return (
        (major, minor, patch + 1),
        (major, minor, patch + 2),
        (major, minor, patch + 3),
    )


def release_target() -> tuple[str, str]:
    if sys.platform == "win32":
        return "x86_64-pc-windows-msvc", "codex-web.exe"
    if sys.platform.startswith("linux"):
        return "x86_64-unknown-linux-gnu", "codex-web"
    raise RegressionFailure(f"unsupported regression platform: {sys.platform}")


def create_release(
    state_directory: Path,
    fixture: Path,
    version: str,
    *,
    fail_readiness: bool = False,
) -> Path:
    target, executable_name = release_target()
    package = state_directory / "updates" / "releases" / f"v{version}"
    (package / "web" / "assets").mkdir(parents=True)
    (package / "THIRD_PARTY_LICENSES").mkdir()
    shutil.copy2(fixture, package / executable_name)
    if os.name != "nt":
        (package / executable_name).chmod(0o700)

    marker = {
        "schemaVersion": 1,
        "product": "codex-web-terminal",
        "version": version,
        "target": target,
    }
    write_json(package / "release-package.json", marker)
    for name in ["README.md", "BUILDING.md", "OPERATIONS.md", "SECURITY.md", "LICENSE"]:
        (package / name).write_text("synthetic updater regression fixture\n", encoding="utf-8")
    (package / "web" / "index.html").write_text(
        '<!doctype html><script type="module" '
        'src="/assets/index-Cq2D_58X.js"></script>'
        '<link rel="stylesheet" href="/assets/index-By1AeVe3.css">\n',
        encoding="utf-8",
    )
    (package / "web" / "assets" / "index-Cq2D_58X.js").write_text(
        "export {};\n",
        encoding="utf-8",
    )
    (package / "web" / "assets" / "index-By1AeVe3.css").write_text(
        "body{}\n",
        encoding="utf-8",
    )
    (package / "THIRD_PARTY_LICENSES" / "THIRD_PARTY_LICENSES.txt").write_text(
        "synthetic fixture\n", encoding="utf-8"
    )
    write_json(package / "THIRD_PARTY_LICENSES" / "manifest.json", {"packages": []})
    if fail_readiness:
        (package / "fixture-fail-readiness").write_text("exit before health\n", encoding="utf-8")
    return package


def write_json(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}-{uuid.uuid4()}.tmp")
    write_json(temporary, value)
    os.replace(temporary, path)


def write_pending(state_directory: Path, source: str, target: str) -> None:
    write_json_atomic(
        state_directory / "updates" / "pending.json",
        {
            "schemaVersion": 1,
            "requestId": str(uuid.uuid4()),
            "sourceVersion": source,
            "targetVersion": target,
        },
    )


def request_restart(state_directory: Path, version: str) -> None:
    write_json_atomic(
        state_directory / CONTROL_DIRECTORY / CONTROL_FILE,
        {"action": "restart", "version": version},
    )


def select_port(requested: int) -> int:
    if requested == LIVE_PORT:
        raise RegressionFailure(f"refusing to use live port {LIVE_PORT}")
    if requested < 0 or requested > 65_535:
        raise RegressionFailure("port must be between 0 and 65535")
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", requested))
        port = listener.getsockname()[1]
    if port == LIVE_PORT:
        return select_port(0)
    return port


def health(port: int) -> dict[str, Any]:
    request = Request(
        f"http://127.0.0.1:{port}/api/health",
        headers={"Authorization": f"Bearer {FIXTURE_TOKEN}"},
    )
    with urlopen(request, timeout=1.0) as response:
        if response.status != 200:
            raise RegressionFailure(f"health returned HTTP {response.status}")
        value = json.load(response)
    if not isinstance(value, dict):
        raise RegressionFailure("health response is not an object")
    return value


def wait_for_generation(
    process: subprocess.Popen[str],
    port: int,
    expected_version: str,
    root_process_id: int,
    timeout: float,
    *,
    reported_root_process_id: int | None = None,
    previous_worker_id: int | None = None,
) -> dict[str, Any]:
    if reported_root_process_id is None:
        reported_root_process_id = root_process_id
    deadline = time.monotonic() + timeout
    last_error = "no health response"
    while time.monotonic() < deadline:
        return_code = process.poll()
        if return_code is not None:
            raise RegressionFailure(
                f"root supervisor {root_process_id} exited unexpectedly with {return_code}"
            )
        try:
            value = health(port)
            worker_id = value.get("processId")
            if (
                value.get("status") == "ok"
                and value.get("serverVersion") == expected_version
                and value.get("rootProcessId") == reported_root_process_id
                and value.get("supervisedWorker") is True
                and isinstance(worker_id, int)
                and worker_id != root_process_id
                and worker_id != previous_worker_id
            ):
                return value
            last_error = f"unexpected health response: {value}"
        except (
            HTTPError,
            URLError,
            TimeoutError,
            OSError,
            RegressionFailure,
            json.JSONDecodeError,
        ) as error:
            last_error = str(error)
        time.sleep(0.05)
    raise RegressionFailure(
        f"generation {expected_version} was not ready within {timeout:.1f}s ({last_error})"
    )


def assert_active(
    state_directory: Path,
    expected_version: str,
    expected_previous_version: str,
) -> None:
    path = state_directory / "updates" / "active.json"
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RegressionFailure(f"active pointer could not be read: {error}") from error
    expected = {
        "schemaVersion": 2,
        "version": expected_version,
        "previousVersion": expected_previous_version,
    }
    if value != expected:
        raise RegressionFailure(f"active pointer mismatch: expected {expected}, got {value}")


def assert_pending_removed(state_directory: Path) -> None:
    path = state_directory / "updates" / "pending.json"
    if path.exists():
        raise RegressionFailure(f"pending activation was not removed: {path}")


def wait_for_committed_active(
    process: subprocess.Popen[str],
    state_directory: Path,
    expected_version: str,
    expected_previous_version: str,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    last_error = "active pointer was not committed"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RegressionFailure(
                f"root supervisor exited before committing {expected_version}"
            )
        try:
            assert_active(
                state_directory,
                expected_version,
                expected_previous_version,
            )
            assert_pending_removed(state_directory)
            return
        except RegressionFailure as error:
            last_error = str(error)
        time.sleep(0.025)
    raise RegressionFailure(
        f"generation {expected_version} did not commit within {timeout:.1f}s ({last_error})"
    )


def wait_for_path_absent(
    process: subprocess.Popen[str],
    path: Path,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RegressionFailure(
                f"root supervisor exited while waiting for cleanup of {path}"
            )
        if not path.exists():
            return
        time.sleep(0.025)
    raise RegressionFailure(f"update package was not cleaned up: {path}")


def stop_process(process: subprocess.Popen[str], current_worker_id: int | None, port: int) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

    worker_still_running = False
    if current_worker_id is not None:
        try:
            worker_still_running = health(port).get("processId") == current_worker_id
        except Exception:
            worker_still_running = False
    if not worker_still_running or current_worker_id is None:
        return

    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(current_worker_id), "/T", "/F"],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    else:
        try:
            os.killpg(current_worker_id, signal.SIGKILL)
        except ProcessLookupError:
            pass


def run_regression(args: argparse.Namespace) -> dict[str, Any]:
    repository = Path(__file__).resolve().parents[1]
    server_directory = repository / "server"
    fixture = (
        args.fixture.resolve()
        if args.fixture
        else build_fixture(server_directory, args.cargo, args.toolchain)
    )
    if not fixture.is_file():
        raise RegressionFailure(f"fixture executable does not exist: {fixture}")

    fixture_version = executable_version(fixture, "fixture")
    root_version = version_text(fixture_version)
    packaged_root = args.root_server is not None
    root_server = args.root_server.resolve() if packaged_root else fixture
    if packaged_root:
        validate_root_server(root_server, root_version)
    first_tuple, second_tuple, failed_tuple = next_versions(fixture_version)
    first_version = version_text(first_tuple)
    second_version = version_text(second_tuple)
    failed_version = version_text(failed_tuple)
    port = select_port(args.port)

    process: subprocess.Popen[str] | None = None
    current_worker_id: int | None = None
    with tempfile.TemporaryDirectory(prefix="cwt-updater-supervisor-") as temporary:
        temporary_directory = Path(temporary)
        project_directory = temporary_directory / "project"
        state_directory = temporary_directory / "state"
        project_directory.mkdir()
        state_directory.mkdir(mode=0o700)

        create_release(state_directory, fixture, first_version)
        write_pending(state_directory, root_version, first_version)

        environment = os.environ.copy()
        environment.pop("CWT_INTERNAL_SUPERVISED_WORKER", None)
        environment.pop("CWT_INTERNAL_READINESS_NONCE", None)
        environment["CODEX_WEB_TOKEN"] = FIXTURE_TOKEN
        environment["HTTP_PROXY"] = "http://127.0.0.1:1"
        environment["HTTPS_PROXY"] = "http://127.0.0.1:1"
        environment["ALL_PROXY"] = "http://127.0.0.1:1"
        environment["NO_PROXY"] = ""
        command = [
            str(root_server),
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--project",
            str(project_directory),
            "--state-dir",
            str(state_directory),
            "--no-open-browser",
            "--update-policy",
            "off",
        ]
        process = subprocess.Popen(
            command,
            cwd=project_directory,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        root_process_id = process.pid
        reported_root_process_id = 0 if packaged_root else root_process_id

        try:
            first = wait_for_generation(
                process,
                port,
                first_version,
                root_process_id,
                args.timeout,
                reported_root_process_id=reported_root_process_id,
            )
            first_worker_id = int(first["processId"])
            current_worker_id = first_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                first_version,
                root_version,
                args.timeout,
            )

            create_release(state_directory, fixture, second_version)
            write_pending(state_directory, first_version, second_version)
            request_restart(state_directory, first_version)
            second = wait_for_generation(
                process,
                port,
                second_version,
                root_process_id,
                args.timeout,
                reported_root_process_id=reported_root_process_id,
                previous_worker_id=first_worker_id,
            )
            second_worker_id = int(second["processId"])
            current_worker_id = second_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                second_version,
                first_version,
                args.timeout,
            )

            create_release(
                state_directory,
                fixture,
                failed_version,
                fail_readiness=True,
            )
            write_pending(state_directory, second_version, failed_version)
            request_restart(state_directory, second_version)
            rollback = wait_for_generation(
                process,
                port,
                second_version,
                root_process_id,
                args.timeout,
                reported_root_process_id=reported_root_process_id,
                previous_worker_id=second_worker_id,
            )
            rollback_worker_id = int(rollback["processId"])
            current_worker_id = rollback_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                second_version,
                first_version,
                args.timeout,
            )

            if not packaged_root:
                root_record = json.loads(
                    (
                        state_directory
                        / CONTROL_DIRECTORY
                        / "root.json"
                    ).read_text(encoding="utf-8")
                )
                if root_record != {"processId": root_process_id}:
                    raise RegressionFailure(
                        f"root supervisor identity changed unexpectedly: {root_record}"
                    )
            if process.poll() is not None:
                raise RegressionFailure("root supervisor exited before the regression completed")

            stop_process(process, current_worker_id, port)
            process.communicate(timeout=2)
            current_worker_id = None

            _, release_executable_name = release_target()
            active_package = (
                state_directory
                / "updates"
                / "releases"
                / f"v{second_version}"
            )
            fail_readiness_marker = active_package / "fixture-fail-readiness"
            fail_readiness_marker.write_text("exit before health\n", encoding="utf-8")

            process = subprocess.Popen(
                command,
                cwd=project_directory,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            runtime_recovery_root_process_id = process.pid
            runtime_recovered = wait_for_generation(
                process,
                port,
                first_version,
                runtime_recovery_root_process_id,
                args.timeout,
                reported_root_process_id=(
                    0 if packaged_root else runtime_recovery_root_process_id
                ),
            )
            runtime_recovered_worker_id = int(runtime_recovered["processId"])
            current_worker_id = runtime_recovered_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                first_version,
                root_version,
                args.timeout,
            )

            wait_for_path_absent(process, active_package, args.timeout)
            create_release(state_directory, fixture, second_version)
            write_pending(state_directory, first_version, second_version)
            request_restart(state_directory, first_version)
            retried_second = wait_for_generation(
                process,
                port,
                second_version,
                runtime_recovery_root_process_id,
                args.timeout,
                reported_root_process_id=(
                    0 if packaged_root else runtime_recovery_root_process_id
                ),
                previous_worker_id=runtime_recovered_worker_id,
            )
            retried_second_worker_id = int(retried_second["processId"])
            current_worker_id = retried_second_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                second_version,
                first_version,
                args.timeout,
            )

            stop_process(process, current_worker_id, port)
            process.communicate(timeout=2)
            current_worker_id = None

            write_pending(state_directory, first_version, second_version)
            corrupt_active_executable = (
                active_package / release_executable_name
            )
            corrupt_active_executable.unlink()

            process = subprocess.Popen(
                command,
                cwd=project_directory,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            recovery_root_process_id = process.pid
            recovered = wait_for_generation(
                process,
                port,
                first_version,
                recovery_root_process_id,
                args.timeout,
                reported_root_process_id=(
                    0 if packaged_root else recovery_root_process_id
                ),
            )
            recovered_worker_id = int(recovered["processId"])
            current_worker_id = recovered_worker_id
            wait_for_committed_active(
                process,
                state_directory,
                first_version,
                root_version,
                args.timeout,
            )

            root_fallback_recovery: dict[str, Any] | None = None
            final_active_version = first_version
            if not packaged_root:
                stop_process(process, current_worker_id, port)
                process.communicate(timeout=2)
                current_worker_id = None

                first_package = (
                    state_directory
                    / "updates"
                    / "releases"
                    / f"v{first_version}"
                )
                (first_package / "fixture-fail-readiness").write_text(
                    "exit before health\n",
                    encoding="utf-8",
                )
                process = subprocess.Popen(
                    command,
                    cwd=project_directory,
                    env=environment,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                root_fallback_supervisor_id = process.pid
                root_fallback = wait_for_generation(
                    process,
                    port,
                    root_version,
                    root_fallback_supervisor_id,
                    args.timeout,
                    reported_root_process_id=root_fallback_supervisor_id,
                )
                root_fallback_worker_id = int(root_fallback["processId"])
                current_worker_id = root_fallback_worker_id
                wait_for_path_absent(
                    process,
                    state_directory / "updates" / "active.json",
                    args.timeout,
                )
                root_fallback_recovery = {
                    "version": root_version,
                    "rootProcessId": root_fallback_supervisor_id,
                    "workerProcessId": root_fallback_worker_id,
                    "activePointerRemoved": True,
                }
                final_active_version = root_version

            return {
                "status": "ok",
                "rootMode": "packaged-server" if packaged_root else "fixture",
                "port": port,
                "rootProcessId": root_process_id,
                "firstActivation": {
                    "version": first_version,
                    "workerProcessId": first_worker_id,
                },
                "secondActivation": {
                    "version": second_version,
                    "workerProcessId": second_worker_id,
                },
                "failedCandidateVersion": failed_version,
                "rollback": {
                    "version": second_version,
                    "workerProcessId": rollback_worker_id,
                },
                "activeVersion": final_active_version,
                "runtimeActiveRecovery": {
                    "version": first_version,
                    "rootProcessId": runtime_recovery_root_process_id,
                    "workerProcessId": runtime_recovered_worker_id,
                    "activePreviousVersion": root_version,
                },
                "reactivatedAfterRuntimeRecovery": {
                    "version": second_version,
                    "workerProcessId": retried_second_worker_id,
                },
                "corruptActiveRecovery": {
                    "version": first_version,
                    "rootProcessId": recovery_root_process_id,
                    "workerProcessId": recovered_worker_id,
                    "activePreviousVersion": root_version,
                },
                "rootFallbackRecovery": root_fallback_recovery,
                "nestedSupervisorObserved": False,
            }
        finally:
            regression_failed = sys.exc_info()[0] is not None
            stop_process(process, current_worker_id, port)
            try:
                stdout, stderr = process.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                stdout, stderr = "", "fixture output remained open after cleanup"
            if regression_failed:
                if stdout.strip():
                    print(stdout.strip()[-4096:], file=sys.stderr)
                if stderr.strip():
                    print(stderr.strip()[-4096:], file=sys.stderr)


def main() -> int:
    args = parse_args()
    try:
        result = run_regression(args)
    except (RegressionFailure, OSError, subprocess.CalledProcessError) as error:
        print(f"updater supervisor regression failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
