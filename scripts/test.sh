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
AFPSQL_TEST_NATIVE_ROOT=""
AFPSQL_TEST_NATIVE_BIN_DIR=""

cleanup_test_database() {
  local status=$?
  local stopped=1
  local validator="${AFDATA_BIN:-afdata}"
  if [ -n "$AFPSQL_TEST_CONTAINER_NAME" ]; then
    container delete --force "$AFPSQL_TEST_CONTAINER_NAME" >/dev/null 2>&1 || true
    AFPSQL_TEST_CONTAINER_NAME=""
  fi
  if [ -n "$AFPSQL_TEST_NATIVE_ROOT" ]; then
    if [ -f "$AFPSQL_TEST_NATIVE_ROOT/data/postmaster.pid" ]; then
      if ! "$AFPSQL_TEST_NATIVE_BIN_DIR/pg_ctl" \
        -D "$AFPSQL_TEST_NATIVE_ROOT/data" -m immediate -w -t 10 stop >/dev/null 2>&1; then
        echo "could not stop the temporary native PostgreSQL; retaining its test directory" >&2
        stopped=0
        if [ "$status" -eq 0 ]; then status=1; fi
      fi
    fi
    if [ "$stopped" -eq 1 ]; then
      if "$validator" guard tmp_path "$AFPSQL_TEST_NATIVE_ROOT" >/dev/null; then
        if ! rm -rf -- "$AFPSQL_TEST_NATIVE_ROOT"; then
          if [ "$status" -eq 0 ]; then status=1; fi
        fi
      else
        if [ "$status" -eq 0 ]; then status=1; fi
      fi
    fi
  fi
  exit "$status"
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

start_native_test_database() {
  local binary port
  local validator="${AFDATA_BIN:-afdata}"
  AFPSQL_TEST_NATIVE_BIN_DIR="${AFPSQL_TEST_POSTGRES_BIN_DIR:-}"
  if [ -z "$AFPSQL_TEST_NATIVE_BIN_DIR" ]; then
    echo "native integration tests require AFPSQL_TEST_POSTGRES_BIN_DIR" >&2
    return 2
  fi
  for binary in initdb pg_ctl createdb postgres; do
    if [ ! -x "$AFPSQL_TEST_NATIVE_BIN_DIR/$binary" ]; then
      echo "native integration fixture lacks PostgreSQL binary: $binary" >&2
      return 2
    fi
  done
  command -v "$validator" >/dev/null 2>&1 || {
    echo "afdata is required to guard the temporary native test directory" >&2
    return 2
  }
  AFPSQL_TEST_NATIVE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/afpsql-test-native.XXXXXX")"
  "$validator" guard tmp_path "$AFPSQL_TEST_NATIVE_ROOT" >/dev/null
  port="$(python3 - <<'PY'
import socket

while True:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    if port != 5432:
        print(port)
        break
PY
)"
  "$AFPSQL_TEST_NATIVE_BIN_DIR/initdb" -D "$AFPSQL_TEST_NATIVE_ROOT/data" \
    -U test --auth=trust --no-locale --encoding=UTF8 >/dev/null
  # Disable Unix sockets so only this randomly selected loopback port is used.
  "$AFPSQL_TEST_NATIVE_BIN_DIR/pg_ctl" -D "$AFPSQL_TEST_NATIVE_ROOT/data" \
    -p "$AFPSQL_TEST_NATIVE_BIN_DIR/postgres" -l "$AFPSQL_TEST_NATIVE_ROOT/postgres.log" \
    -o "-h 127.0.0.1 -p $port -c unix_socket_directories='' -c timezone=UTC" -w -t 60 start >/dev/null
  # Explicit connection fields and absent service variables prevent libpq environment
  # fallbacks from moving this fixture's administrative connection elsewhere.
  PGHOSTADDR=127.0.0.1 \
    PGPASSFILE="$AFPSQL_TEST_NATIVE_ROOT/no-password-file" PGSSLMODE=disable PGCONNECT_TIMEOUT=5 \
    env -u PGSERVICE -u PGSERVICEFILE -u PGPASSWORD \
    "$AFPSQL_TEST_NATIVE_BIN_DIR/createdb" --maintenance-db=postgres \
    -h 127.0.0.1 -p "$port" -U test --no-password test
  export AFPSQL_TEST_DSN_SECRET="postgresql://test@127.0.0.1:${port}/test?sslmode=disable"
  export DATABASE_URL="$AFPSQL_TEST_DSN_SECRET"
  echo "==> local integration PostgreSQL: private native fixture on a random loopback port" >&2
}

ensure_test_database() {
  case "${AFPSQL_TEST_DATABASE_RUNTIME:-}" in
    native)
      start_native_test_database
      return
      ;;
    "") ;;
    *)
      echo "AFPSQL_TEST_DATABASE_RUNTIME must be native or unset" >&2
      return 2
      ;;
  esac
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

check_minimal_cli_discovery() {
  local name binary help version
  local validator="${AFDATA_BIN:-afdata}"
  for name in afpsql afpsql-readonly; do
    binary="${CARGO_TARGET_DIR:-$ROOTPATH/target}/debug/$name"
    if [ ! -x "$binary" ] && [ -x "${binary}.exe" ]; then
      binary="${binary}.exe"
    fi
    help="$("$binary" --help --output json)"
    version="$("$binary" --version --output json)"
    printf '%s\n' "$help" | "$validator" validate - --strict >/dev/null
    printf '%s\n' "$version" | "$validator" validate - --strict >/dev/null
    python3 - "$name" "$help" "$version" <<'PY'
import json
import sys

name, help_text, version_text = sys.argv[1:]
help_event = json.loads(help_text)
assert help_event["kind"] == "result", help_event
assert help_event["result"]["code"] == "help", help_event
help_model = help_event["result"]["help"]
assert help_model["schema"] == "cli-help-v2", help_model
assert help_model["command_path"] == name, help_model
assert f"{name} inspect --help" in help_model["subcommands"], help_model
assert f"{name} ui --help" not in help_model["subcommands"], help_model
version_event = json.loads(version_text)
assert version_event["kind"] == "result", version_event
assert version_event["result"]["code"] == "version", version_event
assert version_event["result"]["name"] == name, version_event
reported_version = version_event["result"]["version"]
assert isinstance(reported_version, str) and reported_version, version_event
PY
  done
}

run_minimal() {
  cargo clippy --no-default-features --all-targets -- -D warnings
  # Several integration tests find the executable beside the test harness.
  # Build both real binaries first so a previous UI build cannot satisfy them.
  cargo build --no-default-features --bins
  if [ -n "$FILTER" ]; then
    cargo test --no-default-features --lib --bins --tests "$FILTER"
  else
    cargo test --no-default-features --lib --bins --tests
  fi
  check_minimal_cli_discovery
}

# The db-tests feature un-ignores every test that needs a live PostgreSQL.
# Without it those tests silently pass as "ignored", which is how a broken
# output contract once reached a release.
run_integration() {
  ensure_test_database
  # Restore the default binaries after minimal mode before database/UI tests.
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
  minimal)       run_minimal ;;
  integration)   run_integration ;;
  container)     run_container ;;
  e2e)           run_integration; run_container ;;
  release-smoke) run_release_smoke ;;
  # `all` is the release gate, so everything that can fail a release has to be
  # able to fail here first. That includes the release smoke, which otherwise
  # runs only inside the Release workflow (i.e. after publishing), and the
  # container e2e, which is the only suite covering transport-boundary and
  # explicit-transaction behavior end to end.
  all)           run_static; run_minimal; run_integration; run_container; run_release_smoke ;;
  *)
    echo "Usage: $0 [static|unit|minimal|integration|container|e2e|release-smoke|all] [FILTER]" >&2
    exit 2
    ;;
esac
