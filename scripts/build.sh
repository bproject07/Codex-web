#!/usr/bin/env bash
set -euo pipefail

require_command() {
  local command_name=$1
  local install_hint=$2

  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'error: %s was not found. %s\n' "$command_name" "$install_hint" >&2
    exit 1
  fi
}

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
project_root=$(cd -- "$script_directory/.." && pwd -P)
web_directory="$project_root/web"
server_directory="$project_root/server"
distribution_directory="$project_root/dist-linux"
expected_distribution_directory="$project_root/dist-linux"

if [[ "$distribution_directory" != "$expected_distribution_directory" ]]; then
  printf 'error: refusing to use an unexpected distribution directory\n' >&2
  exit 1
fi

if [[ -L "$distribution_directory" ]]; then
  printf 'error: refusing to replace a symlinked distribution directory\n' >&2
  exit 1
fi

require_command node 'Install Node.js ^20.19.0 or >=22.12.0.'
require_command npm 'Install npm with Node.js.'
require_command rustc 'Install stable Rust from https://rustup.rs/.'
require_command cargo 'Install stable Rust from https://rustup.rs/.'

if ! node -e '
  const [major, minor] = process.versions.node.split(".").map(Number);
  const supported =
    (major === 20 && minor >= 19) ||
    (major === 22 && minor >= 12) ||
    major > 22;
  if (!supported) process.exit(1);
'; then
  printf 'error: unsupported Node.js version; use ^20.19.0 or >=22.12.0\n' >&2
  exit 1
fi

printf 'Node.js: %s\n' "$(node --version)"
printf 'npm:     %s\n' "$(npm --version)"
printf 'Rust:    %s\n\n' "$(rustc --version)"

(
  cd -- "$web_directory"
  npm ci
  npm run build
)

(
  cd -- "$server_directory"
  cargo build --release --locked
)

built_executable="$server_directory/target/release/codex-web"
frontend_build="$web_directory/dist"

if [[ ! -x "$built_executable" ]]; then
  printf 'error: release executable was not created at %s\n' "$built_executable" >&2
  exit 1
fi

if [[ ! -f "$frontend_build/index.html" ]]; then
  printf 'error: frontend assets were not created at %s\n' "$frontend_build" >&2
  exit 1
fi

if [[ -e "$distribution_directory" ]]; then
  rm -rf -- "$distribution_directory"
fi

install -d "$distribution_directory/web"
install -m 0755 "$built_executable" "$distribution_directory/codex-web"
cp -R "$frontend_build/." "$distribution_directory/web/"

for documentation_file in README.md BUILDING.md OPERATIONS.md AGENTS.md TODO.md LICENSE; do
  cp "$project_root/$documentation_file" "$distribution_directory/"
done

printf '\nBuild complete.\n'
printf 'Executable: %s\n' "$distribution_directory/codex-web"
printf 'Run with:\n'
printf './scripts/run.sh "/home/user/projects/my-app"\n'
