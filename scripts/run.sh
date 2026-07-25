#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./scripts/run.sh PROJECT_DIRECTORY [CODEX_WEB_ARGUMENTS...]

Examples:
  ./scripts/run.sh "/home/user/projects/my-app"
  ./scripts/run.sh "/home/user/projects/my-app" --host 127.0.0.1 --port 8787
  ./scripts/run.sh "/home/user/projects/my-app" --host 100.x.y.z --no-open-browser
EOF
}

if [[ $# -lt 1 ]]; then
  usage >&2
  exit 2
fi

project_argument=$1
shift

for argument in "$@"; do
  if [[ "$argument" == "--project" || "$argument" == --project=* ]]; then
    printf 'error: pass the project only as the first positional argument\n' >&2
    exit 2
  fi
done

if [[ ! -d "$project_argument" ]]; then
  printf 'error: project is not a directory: %s\n' "$project_argument" >&2
  exit 1
fi

project_directory=$(cd -- "$project_argument" && pwd -P)
script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
project_root=$(cd -- "$script_directory/.." && pwd -P)

executable=
for candidate in \
  "$project_root/dist-linux/codex-web" \
  "$project_root/server/target/release/codex-web" \
  "$project_root/server/target/debug/codex-web"; do
  if [[ -x "$candidate" ]]; then
    executable=$candidate
    break
  fi
done

if [[ -z "$executable" ]]; then
  printf 'error: codex-web has not been built. Run ./scripts/build.sh first.\n' >&2
  exit 1
fi

exec "$executable" --project "$project_directory" "$@"
