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
AFPSQL_TEST_CONTAINER_NAME=""

cleanup_test_database() {
  if [ -n "$AFPSQL_TEST_CONTAINER_NAME" ]; then
    container delete --force "$AFPSQL_TEST_CONTAINER_NAME" >/dev/null 2>&1 || true
    AFPSQL_TEST_CONTAINER_NAME=""
  fi
}

trap cleanup_test_database EXIT

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

start_local_test_database() {
  if [ "$(uname -s)" != "Darwin" ]; then
    echo "integration tests require DATABASE_URL or AFPSQL_TEST_DSN_SECRET outside macOS" >&2
    return 1
  fi
  command -v container >/dev/null 2>&1 || {
    echo "Apple container is required for local macOS integration tests" >&2
    return 2
  }
  command -v afdata >/dev/null 2>&1 || {
    echo "afdata is required to inspect the local test container safely" >&2
    return 2
  }

  container system start >/dev/null
  AFPSQL_TEST_CONTAINER_NAME="afpsql-test-pg-${PPID}-$$"
  local image="${AFPSQL_E2E_POSTGRES_IMAGE:-postgres:16}"
  container run \
    --detach \
    --rm \
    --name "$AFPSQL_TEST_CONTAINER_NAME" \
    --env POSTGRES_USER=test \
    --env POSTGRES_PASSWORD=test \
    --env POSTGRES_DB=test \
    "$image" >/dev/null

  local ready="0"
  for _ in {1..120}; do
    if container exec "$AFPSQL_TEST_CONTAINER_NAME" \
      pg_isready -h 127.0.0.1 -p 5432 -U test -d test >/dev/null 2>&1; then
      ready="1"
      break
    fi
    sleep 0.5
  done
  if [ "$ready" != "1" ]; then
    echo "temporary PostgreSQL container did not become ready" >&2
    return 1
  fi

  local address_cidr
  local address
  address_cidr="$(container inspect "$AFPSQL_TEST_CONTAINER_NAME" | afdata value - 0.status.networks.0.ipv4Address)"
  address="${address_cidr%%/*}"
  if [ -z "$address" ]; then
    echo "temporary PostgreSQL container has no IPv4 address" >&2
    return 1
  fi
  export AFPSQL_TEST_DSN_SECRET="postgresql://test:test@${address}:5432/test?sslmode=disable"
  export DATABASE_URL="$AFPSQL_TEST_DSN_SECRET"
  echo "==> local integration PostgreSQL: $AFPSQL_TEST_CONTAINER_NAME (private container address; no host port)" >&2
}

ensure_test_database() {
  load_local_test_env
  if [ -z "${DATABASE_URL:-}" ] && [ -z "${AFPSQL_TEST_DSN_SECRET:-}" ]; then
    start_local_test_database
    return
  fi

  if [ "$(uname -s)" = "Darwin" ]; then
    local configured_dsn
    for configured_dsn in "${DATABASE_URL:-}" "${AFPSQL_TEST_DSN_SECRET:-}"; do
      case "$configured_dsn" in
        *localhost:5432* | *127.0.0.1:5432* | *\[::1\]:5432*)
          echo "refusing to run local integration tests against host PostgreSQL port 5432" >&2
          echo "unset the test DSN to use a disposable Apple container, or configure a separate test port" >&2
          return 2
          ;;
      esac
    done
  fi
}

select_test_container_runtime() {
  local runtime="${AFPSQL_TEST_CONTAINER_RUNTIME:-}"
  if [ -z "$runtime" ]; then
    if [ "${CI:-}" = "true" ]; then
      runtime="docker"
    elif [ "$(uname -s)" = "Darwin" ]; then
      runtime="apple"
    else
      runtime="docker"
    fi
  fi

  case "$runtime" in
    apple)
      command -v container >/dev/null 2>&1 || {
        echo "Apple container is required for local macOS container e2e" >&2
        return 2
      }
      container system start >/dev/null
      ;;
    docker)
      command -v docker >/dev/null 2>&1 || {
        echo "Docker is required for the selected CI container e2e runtime" >&2
        return 2
      }
      docker info >/dev/null
      ;;
    *)
      echo "AFPSQL_TEST_CONTAINER_RUNTIME must be apple or docker" >&2
      return 2
      ;;
  esac
  export AFPSQL_TEST_CONTAINER_RUNTIME="$runtime"
}

run_static() {
  # Parsing is not linting, and this ran neither until now. The pinned version
  # lives in the script itself, mirrored from the monorepo and compared byte for
  # byte there, so this spore and its siblings cannot drift onto different rules.
  "$ROOTPATH/scripts/install-shellcheck.sh" --check "$ROOTPATH"
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
  ensure_test_database
  cargo build
  if [ -n "$FILTER" ]; then
    cargo test --features db-tests --tests "$FILTER"
  else
    cargo test --features db-tests --tests
  fi
}

# Container transport e2e builds its own PostgreSQL and bridge containers. Local
# macOS uses Apple container; CI explicitly selects Docker.
run_container() {
  select_test_container_runtime
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
