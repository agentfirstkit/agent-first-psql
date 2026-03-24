#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

find_repo_root() {
  local dir="$PROJECT_ROOT"

  while [ "$dir" != "/" ]; do
    if [ -f "$dir/scripts/rust-project/lib.sh" ]; then
      printf '%s\n' "$dir"
      return 0
    fi

    dir="$(dirname "$dir")"
  done

  return 1
}

REPO_ROOT="$(find_repo_root)" || {
  echo "Could not locate repository root from $PROJECT_ROOT" >&2
  exit 1
}

SCRIPT_NAME="$(basename "$0")"

export CMN_RUST_PROJECT_ROOT="$PROJECT_ROOT"
export CMN_RUST_SCRIPT_INVOCATION="$0"

exec "$REPO_ROOT/scripts/rust-project/$SCRIPT_NAME" "$@"
