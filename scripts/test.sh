#!/bin/bash
# Fixed test entrypoint for agent-first-psql.
#
# This is the single source of truth for how the project is tested. CI and the
# monorepo release gate both call it, so a mode can never mean one thing locally
# and another thing in CI.
set -euo pipefail

ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOTPATH"

MODE="${1:-all}"
FILTER="${2:-}"

# Integration and e2e talk to a real PostgreSQL. Accept it from the environment
# or from a local dotenv so a developer does not have to export it every time.
load_local_test_env() {
  if [ -n "${DATABASE_URL:-}" ] || [ -n "${AFPSQL_TEST_DSN_SECRET:-}" ]; then
    return
  fi

  local env_file
  for env_file in "$ROOTPATH/tests/.env.local" "$ROOTPATH/tests/.env"; do
    if [ -f "$env_file" ]; then
      set -a
      # shellcheck disable=SC1090
      . "$env_file"
      set +a
      return
    fi
  done
}

require_test_database() {
  load_local_test_env
  if [ -z "${DATABASE_URL:-}" ] && [ -z "${AFPSQL_TEST_DSN_SECRET:-}" ]; then
    echo "integration tests require DATABASE_URL or AFPSQL_TEST_DSN_SECRET" >&2
    echo "set it in tests/.env.local, or in the monorepo at scripts/projects/agent-first-psql/.env.local" >&2
    return 1
  fi
}

run_static() {
  cargo fmt --all --check
  cargo clippy --all-targets -- -D warnings
}

run_unit() {
  if [ -n "$FILTER" ]; then
    cargo test --lib --bins --tests "$FILTER"
  else
    cargo test --lib --bins --tests
  fi
}

# The db-tests feature un-ignores every test that needs a live PostgreSQL.
# Without it those tests silently pass as "ignored", which is how a broken
# output contract once reached a release.
run_integration() {
  require_test_database
  cargo build
  if [ -n "$FILTER" ]; then
    cargo test --features db-tests --tests "$FILTER"
  else
    cargo test --features db-tests --tests
  fi
}

# Container transport e2e builds its own PostgreSQL and bridge containers, so it
# needs Docker rather than the integration database.
run_container() {
  AFPSQL_E2E=1 cargo test --features db-tests --test container_e2e -- --ignored --nocapture
}

run_release_smoke() {
  cargo build --release
  ./scripts/release-smoke.sh target/release
}

case "$MODE" in
  static)        run_static ;;
  unit)          run_unit ;;
  integration)   run_integration ;;
  container)     run_container ;;
  e2e)           run_integration; run_container ;;
  release-smoke) run_release_smoke ;;
  all)           run_static; run_integration ;;
  full)          run_static; run_integration; run_container; run_release_smoke ;;
  *)
    echo "Usage: $0 [static|unit|integration|container|e2e|release-smoke|all|full] [FILTER]" >&2
    exit 2
    ;;
esac
