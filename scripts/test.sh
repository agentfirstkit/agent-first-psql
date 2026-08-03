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
    echo "copy tests/.env.example to tests/.env.local and fill it in, or export the variable" >&2
    echo "(a wrapper that already exports it satisfies this too — this loader only fills the gap)" >&2
    return 1
  fi
}

run_static() {
  cargo fmt --all --check
  cargo clippy --all-targets -- -D warnings
}

# Deliberately no docs/cli.md drift check here. The file is rendered partly by
# agent-first-data, which this repository builds from a local path override
# while CI and the release build it from the published crate, so a byte
# comparison run locally is not authoritative and goes red whenever the sibling
# spore has unreleased edits. The release regenerates the file before the gate
# runs (scripts/release/lib.sh, `projects.sh docs` ahead of Step 0), so what is
# published always matches the binary being published. What is worth asserting
# is a property of the output rather than its bytes — see
# `docs_document_every_exit_code_the_binary_can_produce` in tests/cli_integration.rs.

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
  # `all` is the release gate, so everything that can fail a release has to be
  # able to fail here first. That includes the release smoke, which otherwise
  # runs only inside the Release workflow (i.e. after publishing), and the
  # container e2e, which is the only suite covering transport-boundary and
  # explicit-transaction behavior end to end.
  all)           run_static; run_integration; run_container; run_release_smoke ;;
  *)
    echo "Usage: $0 [static|unit|integration|container|e2e|release-smoke|all] [FILTER]" >&2
    exit 2
    ;;
esac
