#!/usr/bin/env bash
set -euo pipefail

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [[ "${1:-}" == "--version" ]]; then
  printf '%s\n' 'codex-mobile-resize-fixture 1.0.0'
  exit 0
fi

exec node "$script_directory/mobile-resize-tui.js"
