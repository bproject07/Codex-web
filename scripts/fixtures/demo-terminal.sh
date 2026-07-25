#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)

if [[ ${1:-} == "--version" ]]; then
  printf 'codex-web-community-demo 1.0.0\n'
  exit 0
fi

exec node "$script_directory/demo-terminal.js"
